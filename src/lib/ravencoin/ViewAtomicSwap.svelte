<script>
    import { onMount, onDestroy } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { activeSwaps, swapWizardState } from '../stores/swapStores.js';
    import { nostrState, publishOffer, publishHandshakeAccept } from '../stores/nostrStore.js';
    import { addToastNotification } from '../stores/notifications.js';

    let loadError = null;
    let myHempAddresses = [];
    let myRvnAddresses = [];

    let showUnlockModal = false;
    let unlockPassphrase = '';
    let unlockError = '';
    let pendingAction = null;
    let unlockTargetChain = 'hemp';

    function isWalletUnlockError(err) {
        const text = String(err || "").toLowerCase();
        return text.includes("error code: -13")
            || text.includes("walletpassphrase")
            || text.includes("wallet passphrase")
            || text.includes("please enter the wallet passphrase")
            || /wallet.*locked|passphrase|unlock/i.test(text);
    }

    function requestWalletUnlock(actionFn, targetChain = 'hemp') {
        unlockPassphrase = '';
        unlockError = '';
        pendingAction = actionFn;
        unlockTargetChain = targetChain;
        showUnlockModal = true;
    }

    async function handleUnlock() {
        unlockError = '';
        try {
            if (unlockTargetChain === 'rvn') {
                await invoke('rvn_wallet_unlock', { passphrase: unlockPassphrase, timeout: 300 });
            } else {
                await invoke('wallet_unlock_active', { password: unlockPassphrase, duration: 300 });
            }
            showUnlockModal = false;
            if (pendingAction) {
                pendingAction();
            }
        } catch(e) {
            if (String(e).toLowerCase().includes("unencrypted")) {
                showUnlockModal = false;
                if (pendingAction) {
                    pendingAction();
                }
            } else {
                unlockError = String(e);
            }
        }
    }



    // Marketplace Filters
    let filterOfferChain = 'all';
    let filterWantChain = 'all';

    $: filteredOffers = $nostrState.offers.filter(o => {
        if (filterOfferChain !== 'all' && o.offer_chain !== filterOfferChain) return false;
        if (filterWantChain !== 'all' && o.want_chain !== filterWantChain) return false;
        return true;
    });

    async function loadAddresses() {
        try {
            const hempRes = await invoke('get_receive_addresses', { showChange: false });
            myHempAddresses = hempRes.map(a => a.address);
        } catch(e) { console.warn("Failed to load hemp addresses", e); }
        
        try {
            const rvnRes = await invoke('rvn_get_receive_addresses');
            myRvnAddresses = rvnRes.map(a => a.address);
        } catch(e) { console.warn("Failed to load rvn addresses", e); }
    }

    async function loadSwaps() {
        try {
            const swaps = await invoke('swap_list');
            activeSwaps.set(swaps);
        } catch (e) {
            loadError = e.toString();
        }
    }

    onMount(() => {
        loadSwaps();
        loadAddresses();
        const interval = setInterval(loadSwaps, 10000);
        
        // Listen for auto-finalized swaps from nostrStore
        const handleNostrUpdate = () => loadSwaps();
        const handleAutoClaim = (e) => {
            addToastNotification(`🎉 Success! Your offer was accepted and finalized over the P2P network! Swap ${e.detail.swap_id.substring(0, 8)}...`, "success");
        };
        
        window.addEventListener('hemp0x_swap_updated', handleNostrUpdate);
        window.addEventListener('hemp0x_swap_autoclaimed', handleAutoClaim);
        
        return () => {
            clearInterval(interval);
            window.removeEventListener('hemp0x_swap_updated', handleNostrUpdate);
            window.removeEventListener('hemp0x_swap_autoclaimed', handleAutoClaim);
        };
    });

    async function createOffer() {
        try {
            swapWizardState.update(s => ({ ...s, error: null }));
            const result = await invoke('swap_generate_intent', {
                offerChain: $swapWizardState.offerChain,
                offerAmount: parseFloat($swapWizardState.offerAmount),
                offerAsset: $swapWizardState.offerAsset || null,
                offerAddress: $swapWizardState.offerAddress,
                wantChain: $swapWizardState.wantChain,
                wantAmount: parseFloat($swapWizardState.wantAmount),
                wantAsset: $swapWizardState.wantAsset || null,
                wantAddress: $swapWizardState.wantAddress
            });
            swapWizardState.update(s => ({ ...s, generatedCode: result.offer_code, step: 2 }));
            
            // Publish to Decentralized Order Book!
            try {
                await publishOffer(result.offer_code);
                console.log("Published offer to Nostr network!");
            } catch (err) {
                console.warn("Could not publish to Nostr:", err);
            }

            loadSwaps();
        } catch (e) {
            swapWizardState.update(s => ({ ...s, error: e.toString() }));
        }
    }

    async function acceptOffer() {
        try {
            swapWizardState.update(s => ({ ...s, error: null }));
            
            if ($swapWizardState.publicOfferEventId) {
                // Phase 3 Automated Handshake!
                let offerData;
                try {
                    offerData = JSON.parse(atob($swapWizardState.acceptedCode));
                } catch (e) {
                    throw new Error("Invalid offer code");
                }

                await publishHandshakeAccept(
                    $swapWizardState.publicOfferEventId,
                    $swapWizardState.publicOfferPubkey,
                    offerData.swap_id,
                    $swapWizardState.wantAddress,
                    $swapWizardState.offerAddress
                );
                console.log("Published Phase 3 Handshake! Waiting for initiator to lock funds...");
                swapWizardState.update(s => ({ ...s, step: 2 }));
            } else {
                // Fallback to Phase 1 Manual Mode if no Nostr connection (pre-funded offers)
                const result = await invoke('swap_accept_offer', {
                    offerCode: $swapWizardState.acceptedCode,
                    wantAddress: $swapWizardState.wantAddress,
                    offerAddress: $swapWizardState.offerAddress
                });
                swapWizardState.update(s => ({ ...s, step: 2, generatedCode: result }));
            }

            loadSwaps();
        } catch (e) {
            if (isWalletUnlockError(e)) {
                const target = String(e).includes("RVN") ? 'rvn' : 'hemp';
                requestWalletUnlock(acceptOffer, target);
                return;
            }
            swapWizardState.update(s => ({ ...s, error: e.toString() }));
        }
    }

    async function executeSwapAction(action, swapId, extraArg = null) {
        try {
            loadError = null;
            if (extraArg) {
                await invoke(action, { swapId, acceptCode: extraArg });
            } else {
                await invoke(action, { swapId });
            }
            loadSwaps();
        } catch (e) {
            if (isWalletUnlockError(e)) {
                const target = String(e).includes("RVN") ? 'rvn' : 'hemp';
                requestWalletUnlock(() => executeSwapAction(action, swapId, extraArg), target);
                return;
            }
            loadError = e.toString();
        }
    }

    async function handleClaim(swapId) {
        try {
            const res = await invoke('swap_claim', { swapId, providedSecret: null });
            alert(res);
            loadSwaps();
        } catch (e) {
            if (e.toString().includes("provide the secret")) {
                const secret = prompt("Please enter the secret revealed by the counterparty:");
                if (secret) {
                    try {
                        const res2 = await invoke('swap_claim', { swapId, providedSecret: secret });
                        alert(res2);
                        loadSwaps();
                    } catch (e2) {
                        alert(`Error claiming: ${e2}`);
                    }
                }
            } else {
                alert(`Error claiming: ${e}`);
            }
        }
    }

    function openCreate() {
        swapWizardState.set({
            isOpen: true,
            step: 1,
            mode: 'create',
            offerChain: 'hemp',
            offerAmount: 0,
            offerAsset: '',
            offerAddress: myHempAddresses[0] || '',
            wantChain: 'rvn',
            wantAmount: 0,
            wantAsset: '',
            wantAddress: myRvnAddresses[0] || '',
            generatedCode: null,
            acceptedCode: null,
            activeSwapId: null,
            error: null
        });
    }

    function openAccept(publicOffer = null) {
        swapWizardState.set({
            isOpen: true,
            step: 1,
            mode: 'accept',
            offerChain: publicOffer ? publicOffer.offer_chain : 'hemp',
            offerAmount: publicOffer ? publicOffer.offer_amount : 0,
            offerAsset: '',
            offerAddress: '',
            wantChain: publicOffer ? publicOffer.want_chain : 'rvn',
            wantAmount: publicOffer ? publicOffer.want_amount : 0,
            wantAsset: '',
            wantAddress: (publicOffer && publicOffer.want_chain === 'hemp') ? myHempAddresses[0] : (publicOffer && publicOffer.want_chain === 'rvn' ? myRvnAddresses[0] : myHempAddresses[0]) || '',
            generatedCode: null,
            acceptedCode: publicOffer ? publicOffer.content : null,
            activeSwapId: null,
            error: null,
            publicOfferEventId: publicOffer ? publicOffer.id : null,
            publicOfferPubkey: publicOffer ? publicOffer.pubkey : null
        });
    }

    function closeWizard() {
        swapWizardState.update(s => ({ ...s, isOpen: false }));
    }

    function copyCode() {
        navigator.clipboard.writeText($swapWizardState.generatedCode);
    }
</script>

<div class="rvn-dashboard">
    <div class="rvn-header">
        <h1>Atomic Swaps</h1>
        <div>
            <button class="rvn-button" on:click={openCreate}>Create Offer</button>
            <button class="rvn-button" style="background-color: #3498db;" on:click={openAccept}>Accept Offer</button>
        </div>
    </div>

    {#if $swapWizardState.isOpen}
        <div class="rvn-card" style="margin-bottom: 20px; border-color: #ff6b00;">
            <div style="display: flex; justify-content: space-between; align-items: center; margin-bottom: 20px;">
                <h2 style="margin: 0; color: #ff6b00;">
                    {$swapWizardState.mode === 'create' ? 'Create Swap Offer' : 'Accept Swap Offer'}
                </h2>
                <button class="rvn-button" style="background: transparent; color: #888;" on:click={closeWizard}>✕</button>
            </div>

            {#if $swapWizardState.error}
                <div style="background: rgba(231, 76, 60, 0.1); color: #e74c3c; padding: 10px; border-radius: 4px; margin-bottom: 15px;">
                    {$swapWizardState.error}
                </div>
            {/if}

            {#if $swapWizardState.mode === 'create'}
                {#if $swapWizardState.step === 1}
                    <div style="display: grid; grid-template-columns: 1fr 1fr; gap: 20px;">
                        <div>
                            <h3>You Offer:</h3>
                            <label style="display:block; margin-bottom:10px;">
                                Chain:
                                <select bind:value={$swapWizardState.offerChain} style="width:100%; padding:8px; background:#111; color:#eee; border:1px solid #333; margin-top:5px;">
                                    <option value="hemp">Hemp0x</option>
                                    <option value="rvn">Ravencoin</option>
                                </select>
                            </label>
                            <label style="display:block; margin-bottom:10px;">
                                Amount:
                                <input type="number" bind:value={$swapWizardState.offerAmount} style="width:100%; padding:8px; background:#111; color:#eee; border:1px solid #333; margin-top:5px;">
                            </label>

                            <label style="display:block; margin-bottom:10px;">
                                Withdraw From Address:
                                <select bind:value={$swapWizardState.offerAddress} style="width:100%; padding:8px; background:#111; color:#eee; border:1px solid #333; margin-top:5px;">
                                    {#each ($swapWizardState.offerChain === 'hemp' ? myHempAddresses : myRvnAddresses) as addr}
                                        <option value={addr}>{addr}</option>
                                    {/each}
                                </select>
                            </label>
                        </div>
                        <div>
                            <h3>You Receive:</h3>
                            <label style="display:block; margin-bottom:10px;">
                                Chain:
                                <select bind:value={$swapWizardState.wantChain} style="width:100%; padding:8px; background:#111; color:#eee; border:1px solid #333; margin-top:5px;">
                                    <option value="rvn">Ravencoin</option>
                                    <option value="hemp">Hemp0x</option>
                                </select>
                            </label>
                            <label style="display:block; margin-bottom:10px;">
                                Amount:
                                <input type="number" bind:value={$swapWizardState.wantAmount} style="width:100%; padding:8px; background:#111; color:#eee; border:1px solid #333; margin-top:5px;">
                            </label>

                            <label style="display:block; margin-bottom:10px;">
                                Receive To Address:
                                <select bind:value={$swapWizardState.wantAddress} style="width:100%; padding:8px; background:#111; color:#eee; border:1px solid #333; margin-top:5px;">
                                    {#each ($swapWizardState.wantChain === 'hemp' ? myHempAddresses : myRvnAddresses) as addr}
                                        <option value={addr}>{addr}</option>
                                    {/each}
                                </select>
                            </label>
                            
                            <div style="margin-top: 15px; padding: 10px; background: rgba(52, 152, 219, 0.1); border-left: 4px solid #3498db; font-size: 13px;">
                                <strong>💡</strong> Generating an offer will publish your intent to the marketplace. <strong>No funds are locked yet!</strong> Your app will automatically negotiate and securely lock funds only when someone accepts the trade.
                            </div>
                        </div>
                    </div>
                    <button class="rvn-button" style="width: 100%; margin-top: 20px;" on:click={createOffer}>Publish Offer to Marketplace</button>
                {:else if $swapWizardState.step === 2}
                    <div style="text-align: center;">
                        <p style="color: #2ecc71; font-weight: bold;">Success! Your offer is live on the Global Marketplace.</p>
                        <p style="font-size: 13px; color: #aaa;">Your app will automatically finalize the trade when someone accepts it.</p>
                        <p style="font-size: 12px; color: #888;">If you prefer, you can also manually share this code with a counterparty:</p>
                        <textarea readonly rows="6" style="width:100%; padding:10px; background:#111; color:#2ecc71; border:1px solid #333; font-family:monospace; margin-bottom:15px; resize:none;" value={$swapWizardState.generatedCode}></textarea>
                        <button class="rvn-button" on:click={copyCode}>Copy Code</button>
                    </div>
                {/if}
            {:else if $swapWizardState.mode === 'accept'}
                {#if $swapWizardState.step === 1}
                    <div>
                        <p>Paste the offer code from the counterparty:</p>
                        <textarea bind:value={$swapWizardState.acceptedCode} rows="6" style="width:100%; padding:10px; background:#111; color:#eee; border:1px solid #333; font-family:monospace; margin-bottom:15px; resize:none;"></textarea>
                        
                        <label style="display:block; margin-bottom:15px;">
                            Send Funds From Address (Refunds):
                            <select bind:value={$swapWizardState.offerAddress} style="width:100%; padding:8px; background:#111; color:#eee; border:1px solid #333; margin-top:5px;">
                                {#each myHempAddresses as addr}
                                    <option value={addr}>Hemp: {addr}</option>
                                {/each}
                                {#each myRvnAddresses as addr}
                                    <option value={addr}>RVN: {addr}</option>
                                {/each}
                            </select>
                            <small style="color:#888;">Select the address to send funds from (must match the chain you are offering).</small>
                        </label>

                        <label style="display:block; margin-bottom:15px;">
                            Receive Funds To Address:
                            <select bind:value={$swapWizardState.wantAddress} style="width:100%; padding:8px; background:#111; color:#eee; border:1px solid #333; margin-top:5px;">
                                {#each myHempAddresses as addr}
                                    <option value={addr}>Hemp: {addr}</option>
                                {/each}
                                {#each myRvnAddresses as addr}
                                    <option value={addr}>RVN: {addr}</option>
                                {/each}
                            </select>
                            <small style="color:#888;">Select the address to receive funds to (must match the chain you are receiving from).</small>
                        </label>

                        <button class="rvn-button" style="width: 100%; background-color: #3498db;" on:click={acceptOffer}>Accept & Lock Funds</button>
                    </div>
                {:else if $swapWizardState.step === 2}
                    <div style="text-align: center;">
                        <p style="font-weight: bold;">Success! You have locked your funds.</p>
                        {#if $swapWizardState.publicOfferEventId}
                            <p style="color: #3498db;">Your acceptance has been broadcasted. The Initiator's app will automatically finalize the trade shortly!</p>
                        {:else}
                            <p>Share this Accept Code back with the initiator to complete the swap.</p>
                        {/if}
                        <textarea readonly rows="6" style="width:100%; padding:10px; background:#111; color:#3498db; border:1px solid #333; font-family:monospace; margin-bottom:15px; resize:none;" value={$swapWizardState.generatedCode}></textarea>
                        <button class="rvn-button" style="background-color: #3498db;" on:click={copyCode}>Copy Accept Code</button>
                    </div>
                {/if}
            {/if}
        </div>
    {/if}

    <div class="rvn-transactions">
        <h2>Active Swaps</h2>
        {#if loadError}
            <p style="color: #e74c3c;">{loadError}</p>
        {:else if $activeSwaps && $activeSwaps.length > 0}
            <table class="rvn-tx-table">
                <thead>
                    <tr>
                        <th>Status</th>
                        <th>Offer</th>
                        <th>Receive</th>
                        <th>Created</th>
                        <th>Actions</th>
                    </tr>
                </thead>
                <tbody>
                    {#each $activeSwaps as swap}
                        <tr>
                            <td>
                                <span class="rvn-status-badge" style="text-transform: capitalize; {swap.phase === 'completed' || swap.phase === 'claimed' ? 'background: rgba(46, 204, 113, 0.1); color: #2ecc71; border-color: rgba(46, 204, 113, 0.5);' : ''} {swap.phase === 'refunded' ? 'background: rgba(231, 76, 60, 0.1); color: #e74c3c; border-color: rgba(231, 76, 60, 0.5);' : ''}">
                                    {swap.phase}
                                </span>
                            </td>
                            <td>{swap.offer_amount} {swap.offer_asset || (swap.offer_chain === 'rvn' ? 'RVN' : 'HEMP')}</td>
                            <td>{swap.want_amount} {swap.want_asset || (swap.want_chain === 'rvn' ? 'RVN' : 'HEMP')}</td>
                            <td>{new Date(swap.created_at * 1000).toLocaleString()}</td>
                            <td style="display: flex; gap: 8px; align-items: center; flex-wrap: wrap;">
                                {#if swap.phase === 'created'}
                                    <input type="text" id={`accept-code-${swap.swap_id}`} placeholder="Paste Accept Code here..." style="padding:4px; background:#111; color:#eee; border:1px solid #333; width: 150px;">
                                    <button class="rvn-button" style="padding: 4px 8px; font-size: 12px; background: transparent; border: 1px solid #3498db; color: #3498db;" on:click={() => {
                                        const code = document.getElementById(`accept-code-${swap.swap_id}`).value;
                                        if(code) executeSwapAction('swap_finalize_offer', swap.swap_id, code);
                                    }}>Finalize & Lock</button>
                                    <button class="rvn-button" style="padding: 4px 8px; font-size: 12px; background: transparent; border: 1px solid #e74c3c; color: #e74c3c;" on:click={() => executeSwapAction('swap_refund', swap.swap_id)}>Refund</button>
                                {:else if swap.phase === 'accepted' || swap.phase === 'offered'}
                                    <button class="rvn-button" style="padding: 4px 8px; font-size: 12px; background: transparent; border: 1px solid #2ecc71; color: #2ecc71;" on:click={() => handleClaim(swap.swap_id)}>Claim</button>
                                    <button class="rvn-button" style="padding: 4px 8px; font-size: 12px; background: transparent; border: 1px solid #e74c3c; color: #e74c3c;" on:click={() => executeSwapAction('swap_refund', swap.swap_id)}>Refund</button>
                                {:else}
                                    <button class="rvn-button" style="padding: 4px 8px; font-size: 12px; background: transparent; border: 1px solid #888; color: #888;" on:click={() => executeSwapAction('swap_delete', swap.swap_id)}>Delete</button>
                                {/if}
                            </td>
                        </tr>
                    {/each}
                </tbody>
            </table>
        {:else}
            <p style="color: #888; text-align: center; padding: 20px;">No active swaps.</p>
        {/if}
    </div>

    <div class="rvn-transactions" style="margin-top: 20px; border-color: #3498db;">
        <div style="display: flex; justify-content: space-between; align-items: center; margin-bottom: 10px; flex-wrap: wrap; gap: 10px;">
            <div style="display: flex; align-items: center; gap: 15px;">
                <h2 style="color: #3498db; margin: 0;">🌐 Global Public Offers</h2>
                
                <!-- Filters -->
                <div style="display: flex; gap: 10px; align-items: center; background: #111; padding: 4px 8px; border-radius: 8px; border: 1px solid #333;">
                    <small style="color: #888;">Filters:</small>
                    <select bind:value={filterOfferChain} style="background: transparent; color: #eee; border: none; font-size: 12px; cursor: pointer;">
                        <option value="all">Offering Any</option>
                        <option value="hemp">Offering Hemp</option>
                        <option value="rvn">Offering RVN</option>
                    </select>
                    <span style="color: #555;">➔</span>
                    <select bind:value={filterWantChain} style="background: transparent; color: #eee; border: none; font-size: 12px; cursor: pointer;">
                        <option value="all">Wanting Any</option>
                        <option value="hemp">Wanting Hemp</option>
                        <option value="rvn">Wanting RVN</option>
                    </select>
                </div>
            </div>

            <small style="color: #888; background: #111; padding: 4px 8px; border-radius: 12px; border: 1px solid #333;">
                {#if $nostrState.isConnected}
                    <span style="color: #2ecc71;">●</span> P2P Network Connected
                {:else}
                    <span style="color: #e74c3c;">●</span> Connecting...
                {/if}
            </small>
        </div>
        {#if filteredOffers && filteredOffers.length > 0}
            <table class="rvn-tx-table">
                <thead>
                    <tr>
                        <th>They Offer</th>
                        <th>They Want</th>
                        <th>Posted</th>
                        <th>Action</th>
                    </tr>
                </thead>
                <tbody>
                    {#each filteredOffers as publicOffer}
                        <tr style={publicOffer.pubkey === $nostrState.pubkey ? "background: rgba(255, 255, 255, 0.05);" : ""}>
                            <td><strong style="color: #ff6b00;">{publicOffer.offer_amount} {publicOffer.offer_chain.toUpperCase()}</strong></td>
                            <td><strong style="color: #2ecc71;">{publicOffer.want_amount} {publicOffer.want_chain.toUpperCase()}</strong></td>
                            <td style="font-size: 13px; color: #aaa;">{new Date(publicOffer.created_at * 1000).toLocaleString()}</td>
                            <td>
                                {#if publicOffer.pubkey === $nostrState.pubkey}
                                    <span style="font-size: 12px; color: #888; border: 1px solid #444; padding: 4px 8px; border-radius: 4px; background: #222;">Your Offer</span>
                                {:else}
                                    <button class="rvn-button" style="padding: 4px 12px; font-size: 13px; background: rgba(52, 152, 219, 0.1); border: 1px solid #3498db; color: #3498db;" on:click={() => openAccept(publicOffer)}>
                                        Accept Trade
                                    </button>
                                {/if}
                            </td>
                        </tr>
                    {/each}
                </tbody>
            </table>
        {:else if $nostrState.offers && $nostrState.offers.length > 0}
            <p style="color: #888; text-align: center; padding: 20px; font-style: italic;">No offers match your current filters.</p>
        {:else}
            <p style="color: #888; text-align: center; padding: 20px; font-style: italic;">Listening for new offers on the network...</p>
        {/if}
    </div>

    {#if showUnlockModal}
        <button class="rvn-modal-backdrop" aria-label="Close modal" on:click={() => showUnlockModal = false}></button>
        <div class="rvn-modal">
            <h3 style="margin-top:0; color:#3498db;">Unlock {unlockTargetChain === 'rvn' ? 'Ravencoin' : 'Hemp0x'} Wallet</h3>
            <p style="font-size: 14px; color: #aaa;">Your {unlockTargetChain === 'rvn' ? 'Ravencoin' : 'Hemp0x'} wallet is locked. Please enter your passphrase to unlock it for 5 minutes to sign this transaction.</p>
            <input type="password" bind:value={unlockPassphrase} placeholder="Wallet Passphrase" style="width: 100%; padding: 10px; background: #111; color: #eee; border: 1px solid #333; border-radius: 4px; margin-top: 10px; margin-bottom: 15px;" on:keydown={(e) => e.key === 'Enter' && handleUnlock()}>
            {#if unlockError}
                <div style="background: rgba(231, 76, 60, 0.1); color: #e74c3c; padding: 10px; border-radius: 4px; margin-bottom: 15px; font-size: 13px;">
                    {unlockError}
                </div>
            {/if}
            <div style="display: flex; gap: 10px;">
                <button class="rvn-button" style="flex: 1; background: transparent; border: 1px solid #555; color: #ccc;" on:click={() => showUnlockModal = false}>Cancel</button>
                <button class="rvn-button" style="flex: 1; background-color: #3498db;" on:click={handleUnlock}>Unlock</button>
            </div>
        </div>
    {/if}
</div>
