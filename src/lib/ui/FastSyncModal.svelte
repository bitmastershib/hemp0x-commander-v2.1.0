<script>
    import { onMount, onDestroy, createEventDispatcher } from 'svelte';
    import { invoke } from '@tauri-apps/api/core';
    import { listen } from '@tauri-apps/api/event';

    export let chain = 'ravencoin'; // 'hemp0x' | 'ravencoin'

    const dispatch = createEventDispatcher();

    let loading = true;
    let error = null;
    let info = null;
    let progress = null;
    let isWorking = false;
    let currentStage = 'idle'; // 'idle' | 'downloading' | 'verifying' | 'stopping' | 'installing' | 'restarting'
    let statusText = '';
    let unlisten = null;

    const chainLabel = chain === 'ravencoin' ? 'Ravencoin' : 'Hemp0x';
    const primaryColor = chain === 'ravencoin' ? '#ff6b00' : '#00e676';

    function humanBytes(bytes) {
        if (!bytes || bytes <= 0) return '0 B';
        const units = ['B', 'KB', 'MB', 'GB', 'TB'];
        let val = bytes;
        let u = 0;
        while (val >= 1024 && u < units.length - 1) {
            val /= 1024;
            u++;
        }
        return `${val.toFixed(2)} ${units[u]}`;
    }

    function formatTime(seconds) {
        if (!seconds || seconds <= 0) return '--';
        const m = Math.floor(seconds / 60);
        const s = Math.floor(seconds % 60);
        if (m > 60) {
            const h = Math.floor(m / 60);
            const remM = m % 60;
            return `${h}h ${remM}m`;
        }
        return m > 0 ? `${m}m ${s}s` : `${s}s`;
    }

    async function loadInfo() {
        loading = true;
        error = null;
        try {
            info = await invoke('snapshot_get_info', { chain });
            if (info && (info.installed || info.has_database)) {
                try { localStorage.setItem(`${chain}_snapshot_installed`, "true"); } catch {}
            }
            if (info.error) {
                error = info.error;
            }
        } catch (e) {
            error = String(e);
        } finally {
            loading = false;
        }
    }

    async function handleStartDownload() {
        isWorking = true;
        currentStage = 'downloading';
        statusText = 'Starting download...';
        progress = { progress: 0, speed_bps: 0, eta_secs: null, status: 'Connecting...' };

        try {
            await invoke('snapshot_download', { chain });
            currentStage = 'downloaded';
            statusText = 'Download complete and verified!';
            await loadInfo();
        } catch (e) {
            const msg = String(e);
            if (msg.includes('paused') || msg.includes('cancelled')) {
                statusText = 'Download paused. You can resume at any time.';
            } else {
                error = msg;
            }
            await loadInfo();
        } finally {
            isWorking = false;
        }
    }

    async function handleInstall() {
        isWorking = true;
        currentStage = 'stopping';
        statusText = `Stopping ${chainLabel} node safely...`;
        progress = { progress: 10, speed_bps: 0, eta_secs: null, status: statusText };

        try {
            // Stop the node first
            if (chain === 'ravencoin') {
                await invoke('rvn_stop_node_and_wait', { timeoutMs: 30000 });
            } else {
                await invoke('stop_node_and_wait', { timeoutMs: 90000 });
            }

            currentStage = 'installing';
            statusText = 'Installing snapshot database files...';
            progress = { progress: 30, speed_bps: 0, eta_secs: null, status: statusText };

            const msg = await invoke('snapshot_install', { chain });

            currentStage = 'restarting';
            statusText = 'Snapshot installed! Restarting node...';
            progress = { progress: 95, speed_bps: 0, eta_secs: null, status: statusText };

            // Restart node
            if (chain === 'ravencoin') {
                await invoke('rvn_start_node');
            } else {
                await invoke('start_node');
            }

            statusText = msg || `${chainLabel} node restarted with snapshot!`;
            try { localStorage.setItem(`${chain}_snapshot_installed`, "true"); } catch {}
            setTimeout(() => {
                dispatch('complete');
                closeModal();
            }, 2500);

        } catch (e) {
            error = `Installation failed: ${e}`;
            // Try to restart node if stopped
            try {
                if (chain === 'ravencoin') await invoke('rvn_start_node');
                else await invoke('start_node');
            } catch {}
            await loadInfo();
        } finally {
            isWorking = false;
        }
    }

    async function handleCancel() {
        try {
            await invoke('snapshot_cancel', { chain });
            statusText = 'Pausing...';
        } catch (e) {
            console.error(e);
        }
    }

    async function handleDiscard() {
        if (!confirm(`Discard downloaded ${chainLabel} snapshot files to free disk space?`)) return;
        try {
            await invoke('snapshot_discard', { chain });
            await loadInfo();
        } catch (e) {
            error = String(e);
        }
    }

    function closeModal() {
        dispatch('close');
    }

    onMount(async () => {
        await loadInfo();
        unlisten = await listen('snapshot-progress', (event) => {
            if (event.payload.chain === chain) {
                progress = event.payload;
                statusText = progress.status;
                if (progress.stage) currentStage = progress.stage;
            }
        });
    });

    onDestroy(() => {
        if (unlisten) unlisten();
    });
</script>

<div class="snapshot-modal-backdrop" on:click={closeModal} role="presentation"></div>

<div class="snapshot-modal" style="--accent: {primaryColor};">
    <header class="modal-header">
        <div class="title-group">
            <span class="icon">⚡</span>
            <div>
                <h3>Fast Sync — {chainLabel} Blockchain Snapshot</h3>
                <p class="subtitle">Skip days of network syncing by downloading a verified chain snapshot</p>
            </div>
        </div>
        <button class="close-btn" on:click={closeModal}>&times;</button>
    </header>

    <div class="modal-body">
        {#if loading}
            <div class="loading-state">
                <div class="spinner"></div>
                <p>Fetching snapshot details from network...</p>
            </div>
        {:else if error && !info}
            <div class="error-banner">
                <strong>⚠️ Connection Error</strong>
                <p>{error}</p>
                <button class="btn-retry" on:click={loadInfo}>Retry Connection</button>
            </div>
        {:else if info}
            {#if error}
                <div class="error-banner">
                    <p>{error}</p>
                </div>
            {/if}

            {#if info.installed}
                <div style="background: rgba(0, 230, 118, 0.1); border: 1px solid #00e676; border-radius: 6px; padding: 10px 14px; font-size: 0.85rem; color: #a7f3d0; display: flex; align-items: center; gap: 8px;">
                    <span>✓</span>
                    <span>Snapshot blockchain database is already in place on this node.</span>
                </div>
            {/if}

            {#if info.entry}
                <div class="snapshot-card">
                    <div class="info-grid">
                        <div class="info-item">
                            <span class="label">CHAIN HEIGHT</span>
                            <span class="val mono">{info.entry?.height ? info.entry.height.toLocaleString() : '--'}</span>
                        </div>
                        <div class="info-item">
                            <span class="label">SNAPSHOT DATE</span>
                            <span class="val">{info.entry?.date || 'Recent'}</span>
                        </div>
                        <div class="info-item">
                            <span class="label">DOWNLOAD SIZE</span>
                            <span class="val">{humanBytes(info.entry?.size || 0)}</span>
                        </div>
                        <div class="info-item">
                            <span class="label">EXTRACTED SIZE</span>
                            <span class="val">{humanBytes(info.entry?.extracted_size || info.entry?.size * 1.5 || 0)}</span>
                        </div>
                    </div>

                    {#if info.entry?.notes}
                        <div class="notes-box">
                            <strong>Note:</strong> {info.entry.notes}
                        </div>
                    {/if}
                </div>
            {:else if !info.downloaded && !info.partial_bytes}
                <div class="snapshot-card" style="text-align: center; padding: 24px 16px;">
                    <p style="margin: 0; color: #cbd5e1; font-weight: 500;">No snapshot archive is currently available for {chainLabel}.</p>
                    <p style="margin: 8px 0 0 0; color: #94a3b8; font-size: 0.8rem;">Your node will continue synchronizing blocks directly from network peers.</p>
                </div>
            {/if}

            {#if isWorking || progress}
                <div class="progress-box">
                    <div class="progress-header">
                        <span class="status-msg">{statusText || 'Processing...'}</span>
                        {#if progress?.progress !== undefined}
                            <span class="percentage mono">{progress.progress.toFixed(1)}%</span>
                        {/if}
                    </div>

                    <div class="progress-bar-bg">
                        <div class="progress-bar-fill" style="width: {progress?.progress || 0}%;"></div>
                    </div>

                    <div class="progress-stats">
                        {#if progress?.speed_bps > 0}
                            <span>Speed: <strong>{humanBytes(progress.speed_bps)}/s</strong></span>
                        {/if}
                        {#if progress?.eta_secs > 0}
                            <span>ETA: <strong>{formatTime(progress.eta_secs)}</strong></span>
                        {/if}
                        {#if progress?.done_bytes > 0 && progress?.total_bytes > 0}
                            <span class="mono">{humanBytes(progress.done_bytes)} / {humanBytes(progress.total_bytes)}</span>
                        {/if}
                    </div>
                </div>
            {/if}

            <div class="security-note">
                <span class="shield">🛡️</span>
                <p>Snapshots only replace public block databases (<code>blocks</code> and <code>chainstate</code>). Your private wallet keys, settings, and address books are kept 100% untouched.</p>
            </div>
        {/if}
    </div>

    <footer class="modal-footer">
        {#if isWorking}
            {#if currentStage === 'downloading'}
                <button class="btn ghost" on:click={handleCancel}>Pause Download</button>
            {/if}
            <button class="btn primary" disabled>
                <span class="inline-spinner"></span>
                Processing...
            </button>
        {:else if info}
            {#if info.downloaded}
                <div class="action-group">
                    <button class="btn danger-ghost" on:click={handleDiscard} title="Delete download file to free space">Discard File</button>
                    <button class="btn primary" on:click={handleInstall}>
                        ⚡ Install Snapshot Now
                    </button>
                </div>
            {:else if info.partial_bytes > 0}
                <div class="action-group">
                    <button class="btn danger-ghost" on:click={handleDiscard}>Discard Partial</button>
                    <button class="btn primary" on:click={handleStartDownload}>
                        ▶ Resume Download ({humanBytes(info.partial_bytes)})
                    </button>
                </div>
            {:else if info.entry}
                <button class="btn secondary" on:click={closeModal}>Cancel</button>
                <button class="btn primary" on:click={handleStartDownload} disabled={!info.available}>
                    ⚡ Download Snapshot ({humanBytes(info.entry?.size || 0)})
                </button>
            {:else}
                <button class="btn secondary" on:click={closeModal}>Close</button>
            {/if}
        {/if}
    </footer>
</div>

<style>
    .snapshot-modal-backdrop {
        position: fixed;
        inset: 0;
        background: rgba(0, 0, 0, 0.75);
        backdrop-filter: blur(4px);
        z-index: 9998;
    }

    .snapshot-modal {
        position: fixed;
        top: 50%;
        left: 50%;
        transform: translate(-50%, -50%);
        width: 540px;
        max-width: 92vw;
        background: #0f1218;
        border: 1px solid var(--accent, #3498db);
        box-shadow: 0 0 30px rgba(0, 0, 0, 0.8), 0 0 15px var(--accent, #3498db);
        border-radius: 8px;
        z-index: 9999;
        color: #e2e8f0;
        display: flex;
        flex-direction: column;
        overflow: hidden;
        font-family: system-ui, -apple-system, sans-serif;
    }

    .modal-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: 16px 20px;
        background: rgba(255, 255, 255, 0.03);
        border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    }

    .title-group {
        display: flex;
        align-items: center;
        gap: 12px;
    }

    .title-group .icon {
        font-size: 24px;
    }

    .modal-header h3 {
        margin: 0;
        font-size: 1.1rem;
        color: #fff;
        font-weight: 700;
    }

    .subtitle {
        margin: 2px 0 0 0;
        font-size: 0.78rem;
        color: #94a3b8;
    }

    .close-btn {
        background: none;
        border: none;
        color: #94a3b8;
        font-size: 24px;
        cursor: pointer;
        padding: 0;
        line-height: 1;
    }
    .close-btn:hover { color: #fff; }

    .modal-body {
        padding: 20px;
        display: flex;
        flex-direction: column;
        gap: 16px;
    }

    .loading-state {
        text-align: center;
        padding: 30px 0;
        color: #94a3b8;
    }

    .spinner {
        width: 32px;
        height: 32px;
        border: 3px solid rgba(255, 255, 255, 0.1);
        border-top-color: var(--accent);
        border-radius: 50%;
        margin: 0 auto 12px;
        animation: spin 0.8s linear infinite;
    }

    @keyframes spin {
        to { transform: rotate(360deg); }
    }

    .error-banner {
        background: rgba(239, 68, 68, 0.1);
        border: 1px solid #ef4444;
        border-radius: 6px;
        padding: 12px 16px;
        font-size: 0.85rem;
        color: #fca5a5;
    }

    .snapshot-card {
        background: rgba(0, 0, 0, 0.3);
        border: 1px solid rgba(255, 255, 255, 0.08);
        border-radius: 6px;
        padding: 14px;
    }

    .info-grid {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: 12px;
    }

    .info-item {
        display: flex;
        flex-direction: column;
        gap: 2px;
    }

    .info-item .label {
        font-size: 0.68rem;
        color: #64748b;
        letter-spacing: 0.5px;
        font-weight: 600;
    }

    .info-item .val {
        font-size: 0.92rem;
        color: #f1f5f9;
        font-weight: 600;
    }

    .mono { font-family: 'Courier New', Courier, monospace; }

    .notes-box {
        margin-top: 12px;
        padding-top: 10px;
        border-top: 1px solid rgba(255, 255, 255, 0.06);
        font-size: 0.78rem;
        color: #cbd5e1;
    }

    .progress-box {
        background: rgba(15, 23, 42, 0.8);
        border: 1px solid rgba(255, 255, 255, 0.12);
        border-radius: 6px;
        padding: 14px;
        display: flex;
        flex-direction: column;
        gap: 8px;
    }

    .progress-header {
        display: flex;
        justify-content: space-between;
        font-size: 0.82rem;
        font-weight: 600;
    }

    .progress-bar-bg {
        width: 100%;
        height: 8px;
        background: #1e293b;
        border-radius: 4px;
        overflow: hidden;
    }

    .progress-bar-fill {
        height: 100%;
        background: var(--accent);
        transition: width 0.3s ease;
    }

    .progress-stats {
        display: flex;
        justify-content: space-between;
        font-size: 0.75rem;
        color: #94a3b8;
    }

    .security-note {
        display: flex;
        align-items: flex-start;
        gap: 10px;
        background: rgba(255, 255, 255, 0.02);
        border-left: 3px solid var(--accent);
        padding: 10px 12px;
        border-radius: 0 4px 4px 0;
        font-size: 0.76rem;
        color: #94a3b8;
    }

    .security-note p {
        margin: 0;
        line-height: 1.4;
    }

    .modal-footer {
        padding: 14px 20px;
        background: rgba(0, 0, 0, 0.2);
        border-top: 1px solid rgba(255, 255, 255, 0.08);
        display: flex;
        justify-content: flex-end;
        gap: 10px;
    }

    .action-group {
        display: flex;
        gap: 10px;
        width: 100%;
        justify-content: flex-end;
    }

    .btn {
        padding: 8px 16px;
        border-radius: 4px;
        font-size: 0.85rem;
        font-weight: 600;
        cursor: pointer;
        border: none;
        transition: all 0.2s ease;
    }

    .btn.primary {
        background: var(--accent);
        color: #000;
    }
    .btn.primary:hover:not(:disabled) {
        filter: brightness(1.15);
        box-shadow: 0 0 12px var(--accent);
    }
    .btn.primary:disabled {
        opacity: 0.6;
        cursor: not-allowed;
    }

    .btn.secondary {
        background: rgba(255, 255, 255, 0.08);
        color: #e2e8f0;
    }
    .btn.secondary:hover { background: rgba(255, 255, 255, 0.15); }

    .btn.ghost {
        background: transparent;
        border: 1px solid rgba(255, 255, 255, 0.2);
        color: #cbd5e1;
    }

    .btn.danger-ghost {
        background: transparent;
        border: 1px solid #ef4444;
        color: #ef4444;
    }
    .btn.danger-ghost:hover {
        background: rgba(239, 68, 68, 0.15);
    }

    .inline-spinner {
        display: inline-block;
        width: 12px;
        height: 12px;
        border: 2px solid rgba(0, 0, 0, 0.2);
        border-top-color: #000;
        border-radius: 50%;
        animation: spin 0.8s linear infinite;
        margin-right: 6px;
    }
</style>
