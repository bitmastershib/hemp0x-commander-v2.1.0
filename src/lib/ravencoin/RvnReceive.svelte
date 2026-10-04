<script>
    import { onMount } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';

    let addresses = [];
    let isLoading = true;
    let error = null;
    let generating = false;

    async function loadAddresses() {
        try {
            isLoading = true;
            addresses = await invoke('rvn_get_receive_addresses');
        } catch (e) {
            error = e.toString();
        } finally {
            isLoading = false;
        }
    }

    onMount(loadAddresses);

    async function generateNewAddress() {
        generating = true;
        error = null;
        try {
            await invoke('rvn_new_address', { label: "Commander Generated" });
            await loadAddresses();
        } catch (e) {
            error = e.toString();
        } finally {
            generating = false;
        }
    }

    function copyAddress(address) {
        navigator.clipboard.writeText(address);
    }

    async function deleteAddress(address) {
        if (!confirm("Are you absolutely sure you want to hide this address from your list?\n\nBecause of blockchain rules, an address cannot be permanently 'deleted' once generated. This will simply hide it from this list.\n\nAny funds sent to this address in the future will still safely go into your total balance.")) {
            return;
        }
        
        try {
            await invoke('rvn_delete_receive_address', { address });
            await loadAddresses();
        } catch (e) {
            error = e.toString();
        }
    }
</script>

<div class="rvn-dashboard">
    <div class="rvn-header">
        <h1>Receive Ravencoin</h1>
        <button class="rvn-button" on:click={generateNewAddress} disabled={generating}>
            {generating ? 'Generating...' : '+ New Address'}
        </button>
    </div>

    {#if error}
        <div style="background: rgba(231, 76, 60, 0.1); color: #e74c3c; padding: 15px; border-radius: 4px; margin-bottom: 20px; border: 1px solid rgba(231, 76, 60, 0.3);">
            {error}
        </div>
    {/if}

    <div class="rvn-transactions">
        {#if isLoading}
            <p style="color: #888; text-align: center; padding: 20px;">Loading addresses...</p>
        {:else if addresses.length > 0}
            <table class="rvn-tx-table">
                <thead>
                    <tr>
                        <th>Address</th>
                        <th>Label</th>
                        <th>Balance</th>
                        <th style="width: 120px;">Actions</th>
                    </tr>
                </thead>
                <tbody>
                    {#each addresses as addr}
                        <tr>
                            <td style="font-family: monospace;">{addr.address}</td>
                            <td>{addr.label || '-'}</td>
                            <td class="amount receive">{addr.balance} RVN</td>
                            <td style="display: flex; gap: 5px;">
                                <button class="rvn-button" style="padding: 4px 8px; font-size: 12px; background: transparent; border: 1px solid #ff6b00; color: #ff6b00; flex: 1;" on:click={() => copyAddress(addr.address)}>
                                    Copy
                                </button>
                                <button class="rvn-button" style="padding: 4px 8px; background: transparent; border: 1px solid #e74c3c; color: #e74c3c; display: flex; align-items: center; justify-content: center;" title="Hide Address" on:click={() => deleteAddress(addr.address)}>
                                    <svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                                        <polyline points="3 6 5 6 21 6"></polyline>
                                        <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path>
                                        <line x1="10" y1="11" x2="10" y2="17"></line>
                                        <line x1="14" y1="11" x2="14" y2="17"></line>
                                    </svg>
                                </button>
                            </td>
                        </tr>
                    {/each}
                </tbody>
            </table>
        {:else}
            <div style="text-align: center; padding: 40px; color: #888;">
                <p>No receive addresses found.</p>
                <button class="rvn-button" style="margin-top: 15px;" on:click={generateNewAddress}>Generate First Address</button>
            </div>
        {/if}
    </div>
</div>
