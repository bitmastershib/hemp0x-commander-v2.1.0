<script>
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  let swaps = [];
  let pollInterval;

  // Create Offer Form
  let offerChain = "HEMP";
  let offerAmount = 0;
  let offerAsset = "";
  let wantChain = "RVN";
  let wantAmount = 0;
  let wantAsset = "";
  let createError = "";
  let createdOfferCode = "";

  // Accept Offer Form
  let acceptOfferCode = "";
  let acceptError = "";
  let acceptSuccess = "";

  onMount(() => {
    fetchSwaps();
    pollInterval = setInterval(fetchSwaps, 10000); // Poll every 10s
    return () => clearInterval(pollInterval);
  });

  async function fetchSwaps() {
    try {
      swaps = await invoke("swap_list");
    } catch (e) {
      console.error("Failed to list swaps", e);
    }
  }

  async function createOffer() {
    createError = "";
    createdOfferCode = "";
    try {
      const offer = await invoke("swap_create_offer", {
        offerChain: offerChain.toLowerCase(),
        offerAmount: parseFloat(offerAmount),
        offerAsset: offerAsset.trim() || null,
        wantChain: wantChain.toLowerCase(),
        wantAmount: parseFloat(wantAmount),
        wantAsset: wantAsset.trim() || null,
        locktimeHours: 24,
      });
      createdOfferCode = offer.offer_code;
      fetchSwaps();
    } catch (e) {
      createError = e;
    }
  }

  async function acceptOffer() {
    acceptError = "";
    acceptSuccess = "";
    try {
      const acceptance = await invoke("swap_accept_offer", {
        offerCode: acceptOfferCode.trim(),
      });
      acceptSuccess = `Successfully accepted swap! HTLC created.`;
      acceptOfferCode = "";
      fetchSwaps();
    } catch (e) {
      acceptError = e;
    }
  }

  async function claimSwap(swapId) {
    try {
      await invoke("swap_claim", { swapId });
      fetchSwaps();
    } catch (e) {
      alert(`Claim failed: ${e}`);
    }
  }

  async function refundSwap(swapId) {
    try {
      await invoke("swap_refund", { swapId });
      fetchSwaps();
    } catch (e) {
      alert(`Refund failed: ${e}`);
    }
  }

  async function deleteSwap(swapId) {
    try {
      await invoke("swap_delete", { swapId });
      fetchSwaps();
    } catch (e) {
      alert(`Delete failed: ${e}`);
    }
  }

  function copyToClipboard(text) {
    navigator.clipboard.writeText(text);
    alert("Offer code copied to clipboard!");
  }
</script>

<div class="swap-container">
  <div class="header">
    <h2>CROSS-CHAIN ATOMIC SWAPS</h2>
    <p>Trustlessly trade HEMP and RVN assets without an intermediary.</p>
  </div>

  <div class="panels">
    <!-- CREATE OFFER PANEL -->
    <div class="panel create-panel">
      <h3>CREATE OFFER</h3>
      <div class="form-group">
        <span style="display: block; margin-bottom: 0.5rem; color: #a0a0a0; font-size: 0.9rem;">I am offering:</span>
        <div class="input-row">
          <input type="number" step="0.00000001" bind:value={offerAmount} placeholder="Amount" />
          <select bind:value={offerChain}>
            <option value="HEMP">HEMP</option>
            <option value="RVN">RVN</option>
          </select>
          <input type="text" bind:value={offerAsset} placeholder="Asset Name (Optional)" />
        </div>
      </div>

      <div class="form-group">
        <span style="display: block; margin-bottom: 0.5rem; color: #a0a0a0; font-size: 0.9rem;">I want in return:</span>
        <div class="input-row">
          <input type="number" step="0.00000001" bind:value={wantAmount} placeholder="Amount" />
          <select bind:value={wantChain}>
            <option value="RVN">RVN</option>
            <option value="HEMP">HEMP</option>
          </select>
          <input type="text" bind:value={wantAsset} placeholder="Asset Name (Optional)" />
        </div>
      </div>

      <button class="btn primary" on:click={createOffer}>Generate Offer</button>

      {#if createError}
        <div class="error">{createError}</div>
      {/if}

      {#if createdOfferCode}
        <div class="success-box">
          <p>Offer created successfully! Send this code to the counterparty:</p>
          <textarea readonly class="code-box" rows="4">{createdOfferCode}</textarea>
          <button class="btn small" on:click={() => copyToClipboard(createdOfferCode)}>Copy Code</button>
        </div>
      {/if}
    </div>

    <!-- ACCEPT OFFER PANEL -->
    <div class="panel accept-panel">
      <h3>ACCEPT OFFER</h3>
      <p>Paste an offer code from a counterparty to review and accept the trade.</p>
      
      <div class="form-group">
        <textarea bind:value={acceptOfferCode} class="code-box" rows="5" placeholder="Paste Base64 Offer Code here..."></textarea>
      </div>

      <button class="btn secondary" on:click={acceptOffer}>Verify & Accept Swap</button>

      {#if acceptError}
        <div class="error">{acceptError}</div>
      {/if}
      {#if acceptSuccess}
        <div class="success">{acceptSuccess}</div>
      {/if}
    </div>
  </div>

  <!-- ACTIVE SWAPS -->
  <div class="active-swaps">
    <h3>ACTIVE & RECENT SWAPS</h3>
    {#if swaps.length === 0}
      <p class="empty-state">No swaps found.</p>
    {:else}
      <div class="table-container">
        <table>
          <thead>
            <tr>
              <th>Date</th>
              <th>Status</th>
              <th>Offering</th>
              <th>Requesting</th>
              <th>Actions</th>
            </tr>
          </thead>
          <tbody>
            {#each swaps as swap}
              <tr>
                <td>{new Date(swap.created_at * 1000).toLocaleString()}</td>
                <td>
                  <span class={`badge phase-${swap.phase}`}>{swap.phase.toUpperCase()}</span>
                </td>
                <td>
                  {swap.offer_amount} {swap.offer_asset || swap.offer_chain.toUpperCase()}
                </td>
                <td>
                  {swap.want_amount} {swap.want_asset || swap.want_chain.toUpperCase()}
                </td>
                <td class="actions">
                  {#if swap.phase === 'accepted' || swap.phase === 'funded'}
                    <button class="btn small" on:click={() => claimSwap(swap.swap_id)}>Claim</button>
                    <button class="btn small danger" on:click={() => refundSwap(swap.swap_id)}>Refund</button>
                  {/if}
                  {#if swap.phase === 'completed' || swap.phase === 'refunded' || swap.phase === 'created'}
                    <button class="btn small danger" on:click={() => deleteSwap(swap.swap_id)}>Delete</button>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </div>
</div>

<style>
  .swap-container {
    padding: 2rem;
    height: 100%;
    overflow-y: auto;
    color: #e0e0e0;
    font-family: 'Inter', sans-serif;
  }
  .header {
    margin-bottom: 2rem;
    border-bottom: 1px solid rgba(255,255,255,0.1);
    padding-bottom: 1rem;
  }
  .header h2 {
    color: #3498db;
    margin: 0 0 0.5rem 0;
    font-size: 1.5rem;
    letter-spacing: 2px;
  }
  .header p {
    color: #a0a0a0;
    margin: 0;
  }
  .panels {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 2rem;
    margin-bottom: 2rem;
  }
  .panel {
    background: rgba(15, 20, 25, 0.9);
    border: 1px solid rgba(52, 152, 219, 0.3);
    border-radius: 8px;
    padding: 1.5rem;
    box-shadow: 0 4px 15px rgba(0,0,0,0.3);
  }
  .panel h3 {
    margin: 0 0 1.5rem 0;
    color: #ffffff;
    font-size: 1.1rem;
  }
  .form-group {
    margin-bottom: 1.5rem;
  }
  .form-group span {
    display: block;
    margin-bottom: 0.5rem;
    color: #a0a0a0;
    font-size: 0.9rem;
  }
  .input-row {
    display: flex;
    gap: 0.5rem;
  }
  input, select, textarea {
    background: rgba(0,0,0,0.5);
    border: 1px solid rgba(255,255,255,0.2);
    color: white;
    padding: 0.75rem;
    border-radius: 4px;
    outline: none;
    font-family: inherit;
    transition: border-color 0.2s;
  }
  input:focus, select:focus, textarea:focus {
    border-color: #3498db;
  }
  input[type="number"] { width: 100px; }
  select { width: 80px; }
  input[type="text"] { flex-grow: 1; }
  .code-box {
    width: 100%;
    resize: vertical;
    font-family: monospace;
    font-size: 0.85rem;
    color: #3498db;
    background: rgba(0,0,0,0.8);
  }
  .btn {
    padding: 0.75rem 1.5rem;
    border: none;
    border-radius: 4px;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.2s;
  }
  .btn.primary {
    background: #3498db;
    color: white;
  }
  .btn.primary:hover {
    background: #2980b9;
    box-shadow: 0 0 10px rgba(52,152,219,0.5);
  }
  .btn.secondary {
    background: rgba(52, 152, 219, 0.2);
    color: #3498db;
    border: 1px solid #3498db;
    width: 100%;
  }
  .btn.secondary:hover {
    background: rgba(52, 152, 219, 0.4);
  }
  .btn.small {
    padding: 0.4rem 0.8rem;
    font-size: 0.8rem;
  }
  .btn.danger {
    background: rgba(231, 76, 60, 0.2);
    color: #e74c3c;
    border: 1px solid #e74c3c;
  }
  .btn.danger:hover {
    background: rgba(231, 76, 60, 0.4);
  }
  .error {
    color: #e74c3c;
    margin-top: 1rem;
    font-size: 0.9rem;
  }
  .success {
    color: #2ecc71;
    margin-top: 1rem;
    font-size: 0.9rem;
  }
  .success-box {
    margin-top: 1rem;
    padding: 1rem;
    background: rgba(46, 204, 113, 0.1);
    border: 1px solid #2ecc71;
    border-radius: 4px;
  }
  .success-box p {
    margin: 0 0 0.5rem 0;
    color: #2ecc71;
    font-size: 0.9rem;
  }
  
  .active-swaps {
    background: rgba(0, 0, 0, 0.4);
    border: 1px solid rgba(255,255,255,0.1);
    border-radius: 8px;
    padding: 1.5rem;
  }
  .active-swaps h3 {
    margin: 0 0 1rem 0;
    color: #ffffff;
  }
  .table-container {
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th, td {
    padding: 1rem;
    text-align: left;
    border-bottom: 1px solid rgba(255,255,255,0.05);
  }
  th {
    color: #a0a0a0;
    font-weight: 500;
    font-size: 0.85rem;
    text-transform: uppercase;
  }
  td {
    font-size: 0.95rem;
  }
  .badge {
    padding: 0.25rem 0.6rem;
    border-radius: 12px;
    font-size: 0.75rem;
    font-weight: bold;
    letter-spacing: 1px;
  }
  .phase-created { background: rgba(241, 196, 15, 0.2); color: #f1c40f; }
  .phase-accepted { background: rgba(52, 152, 219, 0.2); color: #3498db; }
  .phase-funded { background: rgba(155, 89, 182, 0.2); color: #9b59b6; }
  .phase-claimed { background: rgba(46, 204, 113, 0.2); color: #2ecc71; }
  .phase-completed { background: rgba(46, 204, 113, 0.2); color: #2ecc71; }
  .phase-refunded { background: rgba(231, 76, 60, 0.2); color: #e74c3c; }
  .actions {
    display: flex;
    gap: 0.5rem;
  }
</style>
