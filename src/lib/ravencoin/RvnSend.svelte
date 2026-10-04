<script>
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { rvnWalletInfo } from '../stores/rvnStores.js';

    let address = '';
    let amount = '';
    let isSending = false;
    let previewData = null;
    let error = null;
    let successTxid = null;

    let myRvnAddresses = [];
    let fromAddress = '';

    let showUnlockModal = false;
    let unlockPassphrase = '';
    let unlockError = '';
    let pendingAction = null;

    onMount(async () => {
        try {
            const rvnRes = await invoke('rvn_get_receive_addresses');
            myRvnAddresses = rvnRes;
        } catch(e) { console.warn("Failed to load rvn addresses", e); }
    });

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
            unlockError = String(e);
        }
    }

    async function previewSend() {
        error = null;
        successTxid = null;
        if (!address || !amount) {
            error = "Address and amount are required.";
            return;
        }

        isSending = true;
        try {
            const result = await invoke('rvn_preview_send', {
                address,
                amount: parseFloat(amount)
            });

            if (result.success) {
                previewData = result.data;
            } else {
                error = result.error;
            }
        } catch (e) {
            error = e.toString();
        } finally {
            isSending = false;
        }
    }

    async function confirmSend() {
        error = null;
        isSending = true;
        try {
            const txid = await invoke('rvn_send_rvn', {
                address,
                amount: parseFloat(amount),
                from_address: fromAddress || null
            });
            successTxid = txid;
            previewData = null;
            address = '';
            amount = '';
            
            // Refresh dashboard
            const dashboard = await invoke('rvn_rpc_dashboard');
            rvnWalletInfo.set({
                balance: dashboard.wallet.balance,
                unconfirmed: dashboard.wallet.pending,
                immature: dashboard.wallet.staked,
                status: dashboard.wallet.status,
                transactions: dashboard.tx
            });

        } catch (e) {
            if (isWalletUnlockError(e)) {
                isSending = false;
                requestWalletUnlock(confirmSend);
                return;
            }
            error = e.toString();
        } finally {
            isSending = false;
        }
    }
</script>

<div class="rvn-dashboard">
    <div class="rvn-header">
        <h1>Send Ravencoin</h1>
        <div class="rvn-balance" style="font-size: 20px;">
            Balance: {$rvnWalletInfo.balance} RVN
        </div>
    </div>

    <div class="rvn-card" style="max-width: 600px; margin: 0 auto;">
        {#if successTxid}
            <div style="background: rgba(46, 204, 113, 0.1); color: #2ecc71; padding: 20px; border-radius: 8px; border: 1px solid rgba(46, 204, 113, 0.3); margin-bottom: 20px; text-align: center;">
                <h3>Transaction Sent Successfully</h3>
                <p style="font-family: monospace; font-size: 12px; margin-top: 10px;">TXID: {successTxid}</p>
                <button class="rvn-button" style="margin-top: 15px;" on:click={() => successTxid = null}>Send Another</button>
            </div>
        {:else}
            <div style="display: flex; flex-direction: column; gap: 15px;">
                <label style="color: #aaa;">
                    Withdraw From Address (Optional):
                    <select bind:value={fromAddress} style="width: 100%; padding: 10px; background: #111; color: #eee; border: 1px solid #333; border-radius: 4px; margin-top: 5px;">
                        <option value="">Any Address (Default)</option>
                        {#each myRvnAddresses as item}
                            <option value={item.address}>{item.address} (Bal: {item.balance})</option>
                        {/each}
                    </select>
                </label>

                <label style="color: #aaa;">
                    Pay To:
                    <input type="text" bind:value={address} placeholder="Enter Ravencoin Address (starts with R or r)" style="width: 100%; padding: 10px; background: #111; color: #eee; border: 1px solid #333; border-radius: 4px; margin-top: 5px;">
                </label>

                <label style="color: #aaa;">
                    Amount (RVN):
                    <input type="number" bind:value={amount} min="0" step="0.00000001" placeholder="0.00" style="width: 100%; padding: 10px; background: #111; color: #eee; border: 1px solid #333; border-radius: 4px; margin-top: 5px;">
                </label>

                {#if error}
                    <div style="background: rgba(231, 76, 60, 0.1); color: #e74c3c; padding: 10px; border-radius: 4px; border: 1px solid rgba(231, 76, 60, 0.3);">
                        {error}
                    </div>
                {/if}

                {#if previewData}
                    <div style="background: rgba(255, 107, 0, 0.1); padding: 15px; border-radius: 4px; border: 1px solid rgba(255, 107, 0, 0.3);">
                        <h3 style="margin-top: 0; color: #ff6b00;">Confirm Transaction</h3>
                        <div class="rvn-stat-row"><span>Amount:</span> <span>{previewData.amount} RVN</span></div>
                        <div class="rvn-stat-row"><span>Fee Estimate:</span> <span>{previewData.fee_estimate} RVN</span></div>
                        <div class="rvn-stat-row" style="margin-top: 10px; padding-top: 10px; border-top: 1px solid #333; font-weight: bold;">
                            <span>Total Output:</span> 
                            <span>{(parseFloat(previewData.amount) + parseFloat(previewData.fee_estimate)).toFixed(8)} RVN</span>
                        </div>
                        
                        {#if previewData.warnings && previewData.warnings.length > 0}
                            <div style="color: #e74c3c; margin-top: 10px; font-size: 12px;">
                                {#each previewData.warnings as warning}
                                    <p style="margin: 2px 0;">⚠ {warning}</p>
                                {/each}
                            </div>
                        {/if}

                        <div style="display: flex; gap: 10px; margin-top: 15px;">
                            <button class="rvn-button" style="flex: 1; background: transparent; border: 1px solid #555; color: #ccc;" on:click={() => previewData = null} disabled={isSending}>Cancel</button>
                            <button class="rvn-button" style="flex: 1;" on:click={confirmSend} disabled={isSending || !previewData.validated}>
                                {isSending ? 'Sending...' : 'Confirm & Send'}
                            </button>
                        </div>
                    </div>
                {:else}
                    <button class="rvn-button" style="padding: 12px; font-size: 16px; margin-top: 10px;" on:click={previewSend} disabled={isSending || !address || !amount}>
                        {isSending ? 'Checking...' : 'Preview Send'}
                    </button>
                {/if}
            </div>
        {/if}
    </div>

    {#if showUnlockModal}
        <button class="rvn-modal-backdrop" aria-label="Close modal" on:click={() => showUnlockModal = false}></button>
        <div class="rvn-modal">
            <h3 style="margin-top:0; color:#3498db;">Unlock Wallet</h3>
            <p style="font-size: 14px; color: #aaa;">Your Ravencoin wallet is locked. Please enter your passphrase to unlock it for 5 minutes.</p>
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
