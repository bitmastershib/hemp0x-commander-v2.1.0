<script>
    import { createEventDispatcher } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';

    const dispatch = createEventDispatcher();
    let isChecking = false;
    let error = null;

    async function recheck() {
        isChecking = true;
        error = null;
        try {
            const status = await invoke('rvn_get_binary_status');
            if (status.daemon_exists) {
                await invoke('rvn_start_node');
                dispatch('complete');
            } else {
                error = "Ravencoin binaries still not found. Please ensure ravend and raven-cli are in the src-tauri/binaries folder.";
            }
        } catch (e) {
            error = e.toString();
        } finally {
            isChecking = false;
        }
    }
</script>

<div class="rvn-setup-container">
    <h1 style="color: #ff6b00; margin-bottom: 20px;">Ravencoin Setup Required</h1>
    
    <p>
        To use the Ravencoin features, Commander needs the Ravencoin Core binaries.
    </p>

    <div style="background: #1e1e1e; padding: 20px; border-radius: 8px; border: 1px solid #333; text-align: left; margin-bottom: 30px;">
        <h3 style="margin-top: 0; color: #eee;">Instructions:</h3>
        <ol style="color: #ccc; line-height: 1.6;">
            <li>Download the official Ravencoin binaries for your OS.</li>
            <li>Extract <code>ravend.exe</code> and <code>raven-cli.exe</code>.</li>
            <li>Place them in Commander's binary folder (e.g. <code>src-tauri/binaries/</code>).</li>
            <li>Click the button below to re-check.</li>
        </ol>
    </div>

    {#if error}
        <div style="background: rgba(231, 76, 60, 0.1); color: #e74c3c; padding: 15px; border-radius: 4px; margin-bottom: 20px; border: 1px solid rgba(231, 76, 60, 0.3);">
            {error}
        </div>
    {/if}

    <button class="rvn-button" on:click={recheck} disabled={isChecking}>
        {isChecking ? 'Checking...' : 'I have placed the binaries, re-check'}
    </button>
</div>
