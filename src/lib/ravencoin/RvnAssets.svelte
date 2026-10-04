<script>
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';

    let assets = [];
    let isLoading = true;
    let error = null;

    async function loadAssets() {
        try {
            isLoading = true;
            assets = await invoke('rvn_list_assets');
        } catch (e) {
            error = e.toString();
        } finally {
            isLoading = false;
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
                                <button class="rvn-button" style="padding: 4px 8px; font-size: 12px; background: transparent; border: 1px solid #ff6b00; color: #ff6b00;">
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
