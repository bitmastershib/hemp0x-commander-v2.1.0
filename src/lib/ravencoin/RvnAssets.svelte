<script>
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';

    let assets = [];
    let isLoading = true;
    let error = null;
    let successMessage = null;

    let selectedAsset = null;
    let transferToAddress = '';
    let transferQty = '';
    let isTransferring = false;
    let transferError = null;

    let showUnlockModal = false;
    let unlockPassphrase = '';
    let unlockError = '';
    let pendingAction = null;

    function isWalletUnlockError(err) {
        const text = String(err || "").toLowerCase();
        return text.includes("error code: -13")
            || text.includes("walletpassphrase")
            || text.includes("wallet passphrase")
            || text.includes("please enter the wallet passphrase")
            || /wallet.*locked|passphrase|unlock/i.test(text);
    }

    function requestWalletUnlock(actionFn) {
        unlockPassphrase = '';
        unlockError = '';
        pendingAction = actionFn;
        showUnlockModal = true;
    }

    async function handleUnlock() {
        unlockError = '';
        try {
            await invoke('rvn_wallet_unlock', { passphrase: unlockPassphrase, timeout: 300 });
            showUnlockModal = false;
            if (pendingAction) {
                pendingAction();
            }
        } catch(e) {
            const msg = String(e || "");
            if (msg.toLowerCase().includes("unencrypted")) {
                showUnlockModal = false;
                if (pendingAction) {
                    pendingAction();
                }
                return;
            }
            unlockError = msg;
        }
    }

    async function loadAssets() {
        try {
            isLoading = true;
            error = null;
            assets = await invoke('rvn_list_assets');
        } catch (e) {
            error = e.toString();
        } finally {
            isLoading = false;
        }
    }

    function openTransferModal(asset) {
        selectedAsset = asset;
        transferToAddress = '';
        transferQty = asset.balance.toString();
        transferError = null;
        successMessage = null;
    }

    function closeTransferModal() {
        selectedAsset = null;
        transferToAddress = '';
        transferQty = '';
        transferError = null;
    }

    async function executeTransfer() {
        transferError = null;
        if (!selectedAsset) return;
        if (!transferToAddress || !transferToAddress.trim()) {
            transferError = "Destination address is required.";
            return;
        }
        const qty = parseFloat(transferQty);
        if (isNaN(qty) || qty <= 0) {
            transferError = "Quantity must be greater than zero.";
            return;
        }
        if (qty > selectedAsset.balance) {
            transferError = `Quantity exceeds current balance (${selectedAsset.balance}).`;
            return;
        }

        isTransferring = true;
        try {
            const txid = await invoke('rvn_transfer_asset', {
                assetName: selectedAsset.name,
                qty,
                toAddress: transferToAddress.trim()
            });
            successMessage = `Successfully transferred ${qty} ${selectedAsset.name}! TXID: ${txid}`;
            closeTransferModal();
            await loadAssets();
        } catch (e) {
            if (isWalletUnlockError(e)) {
                isTransferring = false;
                requestWalletUnlock(executeTransfer);
                return;
            }
            transferError = e.toString();
        } finally {
            isTransferring = false;
        }
    }

    onMount(loadAssets);
</script>

<div class="rvn-dashboard">
    <div class="rvn-header">
        <h1>Ravencoin Assets</h1>
        <button class="rvn-button" on:click={loadAssets} disabled={isLoading}>
            Refresh
        </button>
    </div>

    {#if successMessage}
        <div style="background: rgba(46, 204, 113, 0.1); color: #2ecc71; padding: 15px; border-radius: 4px; margin-bottom: 20px; border: 1px solid rgba(46, 204, 113, 0.3);">
            {successMessage}
        </div>
    {/if}

    {#if error}
        <div style="background: rgba(231, 76, 60, 0.1); color: #e74c3c; padding: 15px; border-radius: 4px; margin-bottom: 20px; border: 1px solid rgba(231, 76, 60, 0.3);">
            {error}
        </div>
    {/if}

    <div class="rvn-transactions">
        {#if isLoading}
            <p style="color: #888; text-align: center; padding: 20px;">Loading assets...</p>
        {:else if assets.length > 0}
            <table class="rvn-tx-table">
                <thead>
                    <tr>
                        <th>Asset Name</th>
                        <th>Type</th>
                        <th>Balance</th>
                        <th style="width: 100px;">Actions</th>
                    </tr>
                </thead>
                <tbody>
                    {#each assets as asset}
                        <tr>
                            <td style="font-weight: bold; color: #eee;">{asset.name}</td>
                            <td>
                                <span class="rvn-status-badge" style="text-transform: capitalize; border-color: #555; color: #ccc; background: transparent;">
                                    {asset.type}
                                </span>
                            </td>
                            <td class="amount receive">{asset.balance}</td>
                            <td>
                                <button class="rvn-button" style="padding: 4px 8px; font-size: 12px; background: transparent; border: 1px solid #ff6b00; color: #ff6b00;" on:click={() => openTransferModal(asset)}>
                                    Transfer
                                </button>
                            </td>
                        </tr>
                    {/each}
                </tbody>
            </table>
        {:else}
            <div style="text-align: center; padding: 40px; color: #888;">
                <p>No Ravencoin assets found in this wallet.</p>
                <p style="font-size: 12px; margin-top: 10px;">Create an atomic swap to trade HEMP assets for RVN assets.</p>
            </div>
        {/if}
    </div>
</div>

{#if selectedAsset}
    <button class="rvn-modal-backdrop" aria-label="Close transfer modal" on:click={closeTransferModal}></button>
    <div class="rvn-modal">
        <h3 style="margin-top:0; color:#ff6b00;">Transfer Asset: {selectedAsset.name}</h3>
        <p style="font-size: 13px; color: #aaa;">Balance: <strong style="color: #eee;">{selectedAsset.balance}</strong></p>

        <label style="display:block; margin-top: 15px; font-size: 13px; color: #ccc;">
            Destination RVN Address:
            <input type="text" bind:value={transferToAddress} placeholder="e.g. R..." style="width: 100%; padding: 10px; background: #111; color: #eee; border: 1px solid #333; border-radius: 4px; margin-top: 5px;">
        </label>

        <label style="display:block; margin-top: 15px; font-size: 13px; color: #ccc;">
            Quantity to Send:
            <input type="number" step="any" min="0.00000001" max={selectedAsset.balance} bind:value={transferQty} style="width: 100%; padding: 10px; background: #111; color: #eee; border: 1px solid #333; border-radius: 4px; margin-top: 5px;">
        </label>

        {#if transferError}
            <div style="background: rgba(231, 76, 60, 0.1); color: #e74c3c; padding: 10px; border-radius: 4px; margin-top: 15px; font-size: 13px;">
                {transferError}
            </div>
        {/if}

        <div style="display: flex; gap: 10px; margin-top: 20px;">
            <button class="rvn-button" style="flex: 1; background: transparent; border: 1px solid #555; color: #ccc;" on:click={closeTransferModal} disabled={isTransferring}>Cancel</button>
            <button class="rvn-button" style="flex: 1; background-color: #ff6b00;" on:click={executeTransfer} disabled={isTransferring}>
                {isTransferring ? 'Sending...' : 'Confirm Transfer'}
            </button>
        </div>
    </div>
{/if}

{#if showUnlockModal}
    <button class="rvn-modal-backdrop" aria-label="Close unlock modal" on:click={() => showUnlockModal = false}></button>
    <div class="rvn-modal">
        <h3 style="margin-top:0; color:#ff6b00;">Unlock Ravencoin Wallet</h3>
        <p style="font-size: 14px; color: #aaa;">Your Ravencoin wallet is locked. Please enter your passphrase to transfer this asset.</p>
        <input type="password" bind:value={unlockPassphrase} placeholder="Wallet Passphrase" style="width: 100%; padding: 10px; background: #111; color: #eee; border: 1px solid #333; border-radius: 4px; margin-top: 10px; margin-bottom: 15px;" on:keydown={(e) => e.key === 'Enter' && handleUnlock()}>
        {#if unlockError}
            <div style="background: rgba(231, 76, 60, 0.1); color: #e74c3c; padding: 10px; border-radius: 4px; margin-bottom: 15px; font-size: 13px;">
                {unlockError}
            </div>
        {/if}
        <div style="display: flex; gap: 10px;">
            <button class="rvn-button" style="flex: 1; background: transparent; border: 1px solid #555; color: #ccc;" on:click={() => showUnlockModal = false}>Cancel</button>
            <button class="rvn-button" style="flex: 1; background-color: #ff6b00;" on:click={handleUnlock}>Unlock</button>
        </div>
    </div>
{/if}
