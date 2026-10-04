<script>
    import { onMount, onDestroy } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { rvnNodeStatus, rvnWalletInfo, rvnDaemonRuntime } from '../stores/rvnStores.js';
    import RvnNodeSetup from './RvnNodeSetup.svelte';
    import RvnSend from './RvnSend.svelte';
    import RvnReceive from './RvnReceive.svelte';
    import ViewAtomicSwap from './ViewAtomicSwap.svelte';
    import { listen } from '@tauri-apps/api/event';
    import './ravencoin.css';

    let activeRvnTab = 'DASHBOARD';

    let updateInterval;
    let isLoading = true;
    let setupRequired = false;
    let snapshotProgress = null;
    let isDownloadingSnapshot = false;
    let isChecking = false;

    let showEncryptModal = false;
    let encryptPassphrase = '';
    let encryptPassphraseConfirm = '';
    let encryptError = '';

    let showUnlockModal = false;
    let unlockPassphrase = '';
    let unlockError = '';

    async function handleUnlock() {
        unlockError = '';
        try {
            await invoke('rvn_wallet_unlock', { passphrase: unlockPassphrase, timeout: 300 });
            showUnlockModal = false;
            checkStatus();
        } catch(e) {
            unlockError = String(e);
        }
    }

    async function handleManualLockToggle() {
        if ($rvnWalletInfo.status === 'UNLOCKED') {
            await invoke('rvn_wallet_lock');
            checkStatus();
        } else if ($rvnWalletInfo.status === 'LOCKED') {
            unlockPassphrase = '';
            unlockError = '';
            showUnlockModal = true;
        }
    }

    async function handleEncrypt() {
        encryptError = '';
        if (!encryptPassphrase) {
            encryptError = 'Passphrase is required.';
            return;
        }
        if (encryptPassphrase !== encryptPassphraseConfirm) {
            encryptError = 'Passphrases do not match.';
            return;
        }

        try {
            await invoke('rvn_wallet_encrypt', { passphrase: encryptPassphrase });
            showEncryptModal = false;
            // The daemon will shut down. Force a status check loop to restart it.
            isLoading = true;
            let checks = 0;
            const restartInterval = setInterval(async () => {
                checks++;
                await checkStatus();
                if ($rvnDaemonRuntime.rpc_ready || checks > 10) {
                    clearInterval(restartInterval);
                }
            }, 2000);
        } catch(e) {
            encryptError = String(e);
        }
    }

    async function checkStatus() {
        if (isDownloadingSnapshot || isChecking) return;
        isChecking = true;
        try {
            // First check if daemon process is running
            const runtime = await invoke('rvn_get_runtime_status');
            rvnDaemonRuntime.set(runtime);

            if (!runtime.running) {
                // If not running, see if binaries exist
                const binStatus = await invoke('rvn_get_binary_status');
                if (!binStatus.daemon_exists) {
                    setupRequired = true;
                } else {
                    // Try to start it
                    await invoke('rvn_start_node');
                    setupRequired = false;
                }
            } else if (runtime.rpc_ready) {
                // Fetch dashboard data
                setupRequired = false;
                const dashboard = await invoke('rvn_rpc_dashboard');
                
                rvnNodeStatus.set({
                    online: true,
                    version: runtime.version,
                    connections: dashboard.node.peers,
                    headers: dashboard.node.headers,
                    blocks: dashboard.node.blocks,
                    synced: dashboard.node.synced,
                    verificationProgress: 1.0, // Simplification
                    error: null
                });

                rvnWalletInfo.set({
                    balance: dashboard.wallet.balance,
                    unconfirmed: dashboard.wallet.pending,
                    immature: dashboard.wallet.staked,
                    status: dashboard.wallet.status,
                    transactions: dashboard.tx
                });
            }
        } catch (e) {
            console.error("RVN status error:", e);
            rvnNodeStatus.update(s => ({ ...s, online: false, error: e.toString() }));
        } finally {
            isLoading = false;
            isChecking = false;
        }
    }

    async function startSnapshot() {
        isDownloadingSnapshot = true;
        snapshotProgress = { status: "Starting download...", progress: 0.0 };
        
        try {
            // Stop node first so we can extract into data dir
            await invoke('rvn_stop_node');
            
            const msg = await invoke('rvn_download_snapshot');
            snapshotProgress = { status: msg, progress: 100.0 };
            
            // Wait 2 seconds, then reboot
            setTimeout(async () => {
                isDownloadingSnapshot = false;
                snapshotProgress = null;
                await invoke('rvn_start_node');
                checkStatus();
            }, 2000);
        } catch (e) {
            snapshotProgress = { status: `Error: ${e}`, progress: 0.0 };
            setTimeout(() => {
                isDownloadingSnapshot = false;
                snapshotProgress = null;
            }, 5000);
        }
    }

    onMount(async () => {
        const unlisten = await listen('snapshot-progress', (event) => {
            snapshotProgress = event.payload;
        });
        
        checkStatus();
        updateInterval = setInterval(checkStatus, 5000);
        
        return () => {
            unlisten();
        };
    });

    onDestroy(() => {
        if (updateInterval) clearInterval(updateInterval);
    });
</script>

{#if isLoading}
    <div class="rvn-setup-container">
        <h2>Connecting to Ravencoin...</h2>
    </div>
{:else if setupRequired}
    <RvnNodeSetup on:complete={checkStatus} />
{:else}
    <div class="rvn-dashboard">
        <div class="rvn-header">
            <div style="display: flex; align-items: center; gap: 20px;">
                <h1>Ravencoin Dashboard</h1>
                <div class="rvn-inner-tabs" style="display: flex; gap: 15px; margin-top: 5px;">
                    <button style="background: none; border: none; font-weight: bold; cursor: pointer; padding: 5px 0; font-size: 14px; {activeRvnTab === 'DASHBOARD' ? 'color: #ff6b00; border-bottom: 2px solid #ff6b00;' : 'color: #888;'}" on:click={() => activeRvnTab = 'DASHBOARD'}>DASHBOARD</button>
                    <button style="background: none; border: none; font-weight: bold; cursor: pointer; padding: 5px 0; font-size: 14px; {activeRvnTab === 'SEND' ? 'color: #ff6b00; border-bottom: 2px solid #ff6b00;' : 'color: #888;'}" on:click={() => activeRvnTab = 'SEND'}>SEND</button>
                    <button style="background: none; border: none; font-weight: bold; cursor: pointer; padding: 5px 0; font-size: 14px; {activeRvnTab === 'RECEIVE' ? 'color: #ff6b00; border-bottom: 2px solid #ff6b00;' : 'color: #888;'}" on:click={() => activeRvnTab = 'RECEIVE'}>RECEIVE</button>
                    <button style="background: none; border: none; font-weight: bold; cursor: pointer; padding: 5px 0; font-size: 14px; {activeRvnTab === 'SWAPS' ? 'color: #3498db; border-bottom: 2px solid #3498db;' : 'color: #888;'}" on:click={() => activeRvnTab = 'SWAPS'}>SWAPS</button>
                </div>
            </div>
            <div style="display: flex; flex-direction: column; align-items: flex-end; gap: 5px;">
                <div style="display: flex; align-items: center; gap: 10px;">
                    {#if $rvnWalletInfo.status !== '--' && $rvnWalletInfo.status !== 'UNENCRYPTED'}
                        <div class="rvn-status-badge" style="background: {$rvnWalletInfo.status === 'UNLOCKED' ? 'rgba(46, 204, 113, 0.1)' : 'rgba(231, 76, 60, 0.1)'}; color: {$rvnWalletInfo.status === 'UNLOCKED' ? '#2ecc71' : '#e74c3c'}; border-color: {$rvnWalletInfo.status === 'UNLOCKED' ? '#2ecc71' : '#e74c3c'}; display: flex; align-items: center; gap: 5px; cursor: pointer;" on:click={handleManualLockToggle} title="Click to {$rvnWalletInfo.status === 'UNLOCKED' ? 'lock' : 'unlock'} wallet">
                            {#if $rvnWalletInfo.status === 'UNLOCKED'}
                                🔓 RVN Wallet Unlocked
                            {:else}
                                🔒 RVN Wallet Locked
                            {/if}
                        </div>
                    {/if}
                    <div class="rvn-status-badge" class:synced={$rvnNodeStatus.synced}>
                        {#if $rvnDaemonRuntime.running && !$rvnDaemonRuntime.rpc_ready}
                            Starting / Verifying Blocks...
                        {:else}
                            {$rvnNodeStatus.synced ? '✓ Synced to RVN Node' : 'Syncing to RVN Node...'} 
                            ({$rvnNodeStatus.blocks} / {$rvnNodeStatus.headers})
                        {/if}
                    </div>
                </div>
                {#if $rvnDaemonRuntime.rpc_ready && !$rvnNodeStatus.synced && $rvnNodeStatus.blocks < 10000 && !isDownloadingSnapshot}
                    <button class="rvn-button" style="font-size: 11px; padding: 4px 8px; background: transparent; border: 1px solid #3498db; color: #3498db;" on:click={startSnapshot}>
                        ⚡ Fast Sync (Snapshot)
                    </button>
                {/if}
            </div>
        </div>

        {#if $rvnWalletInfo.status === 'UNENCRYPTED'}
            <div style="background: rgba(231, 76, 60, 0.1); border-left: 4px solid #e74c3c; padding: 12px 20px; margin-bottom: 20px; display: flex; justify-content: space-between; align-items: center;">
                <div>
                    <h3 style="margin: 0 0 5px 0; color: #e74c3c;">⚠️ Security Warning</h3>
                    <p style="margin: 0; font-size: 13px; color: #ccc;">Your Ravencoin wallet is completely unencrypted. Anyone with access to your computer can steal your RVN.</p>
                </div>
                <button class="rvn-button" style="background: #e74c3c; color: white;" on:click={() => { encryptPassphrase = ''; encryptPassphraseConfirm = ''; encryptError = ''; showEncryptModal = true; }}>Encrypt Wallet Now</button>
            </div>
        {/if}

        {#if activeRvnTab === 'DASHBOARD'}
            {#if isDownloadingSnapshot && snapshotProgress}
                <div class="rvn-card" style="margin-bottom: 20px; border-color: #3498db;">
                    <h3 style="margin-top: 0; color: #3498db;">Downloading Blockchain Snapshot</h3>
                    <p>{snapshotProgress.status}</p>
                    <div style="width: 100%; height: 10px; background: #111; border-radius: 5px; overflow: hidden; margin-top: 10px;">
                        <div style="height: 100%; background: #3498db; width: {snapshotProgress.progress}%; transition: width 0.3s ease;"></div>
                    </div>
                </div>
            {/if}

            <div class="rvn-cards">
            <div class="rvn-card">
                <h2>Wallet Balance</h2>
                <div class="rvn-balance">
                    {$rvnWalletInfo.balance} <small>RVN</small>
                </div>
                <div class="rvn-stats" style="margin-top: 15px;">
                    <div class="rvn-stat-row">
                        <span class="rvn-stat-label">Pending</span>
                        <span class="rvn-stat-value">{$rvnWalletInfo.unconfirmed} RVN</span>
                    </div>
                </div>
            </div>

            <div class="rvn-card">
                <h2>Node Information</h2>
                <div class="rvn-stats">
                    <div class="rvn-stat-row">
                        <span class="rvn-stat-label">Connections</span>
                        <span class="rvn-stat-value">{$rvnNodeStatus.connections} peers</span>
                    </div>
                    <div class="rvn-stat-row">
                        <span class="rvn-stat-label">Version</span>
                        <span class="rvn-stat-value">{$rvnNodeStatus.version}</span>
                    </div>
                    <div class="rvn-stat-row">
                        <span class="rvn-stat-label">Data Directory</span>
                        <span class="rvn-stat-value" style="font-size: 11px;">{$rvnDaemonRuntime.data_dir}</span>
                    </div>
                </div>
            </div>
        </div>

        <div class="rvn-transactions">
            <h2>Recent Transactions</h2>
            {#if $rvnWalletInfo.transactions && $rvnWalletInfo.transactions.length > 0}
                <table class="rvn-tx-table">
                    <thead>
                        <tr>
                            <th>Date</th>
                            <th>Type</th>
                            <th>Amount</th>
                            <th>Asset</th>
                            <th>Confirmations</th>
                            <th>TXID</th>
                        </tr>
                    </thead>
                    <tbody>
                        {#each $rvnWalletInfo.transactions as tx}
                            <tr>
                                <td>{tx.date}</td>
                                <td style="text-transform: capitalize;">{tx.type}</td>
                                <td class="amount {tx.type}">
                                    {tx.amount}
                                </td>
                                <td>{tx.asset || 'RVN'}</td>
                                <td>{tx.conf}</td>
                                <td class="txid">{tx.txid.substring(0, 16)}...</td>
                            </tr>
                        {/each}
                    </tbody>
                </table>
            {:else}
                <p style="color: #888; padding: 20px 0; text-align: center;">No recent transactions found.</p>
            {/if}
        </div>
        {:else if activeRvnTab === 'SEND'}
            <RvnSend />
        {:else if activeRvnTab === 'RECEIVE'}
            <RvnReceive />
        {:else if activeRvnTab === 'SWAPS'}
            <ViewAtomicSwap />
        {/if}
    </div>

    {#if showEncryptModal}
        <button class="rvn-modal-backdrop" aria-label="Close modal" on:click={() => showEncryptModal = false}></button>
        <div class="rvn-modal">
            <h3 style="margin-top:0; color:#e74c3c;">Encrypt Ravencoin Wallet</h3>
            <p style="font-size: 14px; color: #aaa;">Set a passphrase to secure your RVN. This passphrase will be required to send funds or execute Atomic Swaps.</p>
            <div style="padding: 10px; background: rgba(231, 76, 60, 0.1); border-left: 3px solid #e74c3c; margin-bottom: 15px;">
                <strong style="color: #e74c3c; font-size: 13px;">WARNING:</strong>
                <span style="font-size: 13px; color: #ccc;">If you forget this passphrase, you will permanently lose access to your RVN. Keep it safe!</span>
            </div>
            
            <input type="password" bind:value={encryptPassphrase} placeholder="New Passphrase" style="width: 100%; padding: 10px; background: #111; color: #eee; border: 1px solid #333; border-radius: 4px; margin-bottom: 10px;">
            <input type="password" bind:value={encryptPassphraseConfirm} placeholder="Confirm Passphrase" style="width: 100%; padding: 10px; background: #111; color: #eee; border: 1px solid #333; border-radius: 4px; margin-bottom: 15px;" on:keydown={(e) => e.key === 'Enter' && handleEncrypt()}>
            
            {#if encryptError}
                <div style="background: rgba(231, 76, 60, 0.1); color: #e74c3c; padding: 10px; border-radius: 4px; margin-bottom: 15px; font-size: 13px;">
                    {encryptError}
                </div>
            {/if}
            <div style="display: flex; gap: 10px;">
                <button class="rvn-button" style="flex: 1; background: transparent; border: 1px solid #555; color: #ccc;" on:click={() => showEncryptModal = false}>Cancel</button>
                <button class="rvn-button" style="flex: 1; background-color: #e74c3c;" on:click={handleEncrypt}>Encrypt & Restart</button>
            </div>
        </div>
    {/if}

    {#if showUnlockModal}
        <button class="rvn-modal-backdrop" aria-label="Close modal" on:click={() => showUnlockModal = false}></button>
        <div class="rvn-modal">
            <h3 style="margin-top:0; color:#3498db;">Unlock Ravencoin Wallet</h3>
            <p style="font-size: 14px; color: #aaa;">Enter your passphrase to manually unlock your wallet for 5 minutes.</p>
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
{/if}
