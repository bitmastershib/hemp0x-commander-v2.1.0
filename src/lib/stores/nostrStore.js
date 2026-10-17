import { writable, get } from 'svelte/store';
import { SimplePool, generateSecretKey, getPublicKey, finalizeEvent } from 'nostr-tools';
import { invoke } from '@tauri-apps/api/core';

// Helper to convert hex to Uint8Array and back for local storage
function hexToBytes(hex) {
    let bytes = new Uint8Array(hex.length / 2);
    for (let i = 0; i < bytes.length; i++) {
        bytes[i] = parseInt(hex.substr(i * 2, 2), 16);
    }
    return bytes;
}

function bytesToHex(bytes) {
    return Array.from(bytes).map(b => b.toString(16).padStart(2, '0')).join('');
}

const RELAYS = [
    'wss://relay.primal.net',
    'wss://nos.lol',
    'wss://nostr.mom',
    'wss://offchain.pub',
    'wss://relay.damus.io'
];

export const nostrState = writable({
    isConnected: false,
    pubkey: null,
    offers: [], // Array of { id, pubkey, created_at, content, offer_chain, want_chain, offer_amount, want_amount, rawData }
    pendingAccepts: [] // Array of { swap_id, offer_chain, offer_amount, want_chain, want_amount, created_at }
});

let pool;
let privKeyBytes;
let pubKeyHex;
let pollTimer = null;
let initialRelaySyncDone = false;
const relaySockets = new Map();
const inFlightEvents = new Set();
const closedSwapsMem = new Set();
const processedEventsMem = new Set();
const syncedIntentSwaps = new Set();
const reconciledClaimSwaps = new Set();
const lastCounterBroadcastAt = new Map();
let isReconcilingSwaps = false;
let pollTicks = 0;

function loadPersistedSets() {
    try {
        const closed = JSON.parse(localStorage.getItem('hemp0x_closed_swaps') || '[]');
        if (Array.isArray(closed)) {
            for (const id of closed) closedSwapsMem.add(id);
        }
    } catch {}
    try {
        const proc = JSON.parse(localStorage.getItem('hemp0x_nostr_processed_events') || '[]');
        if (Array.isArray(proc)) {
            for (const id of proc) processedEventsMem.add(id);
        }
    } catch {}
}

function getProcessedEvents() {
    if (processedEventsMem.size === 0) loadPersistedSets();
    return processedEventsMem;
}

function markEventProcessed(eventId) {
    if (!eventId) return;
    processedEventsMem.add(eventId);
    try {
        const arr = Array.from(processedEventsMem).slice(-500);
        localStorage.setItem('hemp0x_nostr_processed_events', JSON.stringify(arr));
    } catch {}
}

function getClosedSwaps() {
    if (closedSwapsMem.size === 0) loadPersistedSets();
    return closedSwapsMem;
}

export function markSwapClosed(swapId) {
    if (!swapId) return;
    closedSwapsMem.add(swapId);
    try {
        const arr = Array.from(closedSwapsMem).slice(-500);
        localStorage.setItem('hemp0x_closed_swaps', JSON.stringify(arr));
    } catch {}
}

export function isSwapClosed(swapId) {
    if (!swapId) return false;
    return getClosedSwaps().has(swapId);
}

function refreshPendingAcceptsStore() {
    try {
        const map = JSON.parse(localStorage.getItem('hemp0x_pending_accepts') || '{}');
        const nowSec = Math.floor(Date.now() / 1000);
        const list = Object.entries(map)
            .filter(([_, v]) => v && !v.funded && (!v.created_at || (nowSec - v.created_at) < 3600))
            .map(([swap_id, v]) => ({
                swap_id,
                phase: 'handshake',
                offer_chain: v.offer_chain || 'rvn',
                offer_amount: v.offer_amount || 0,
                want_chain: v.want_chain || 'hemp',
                want_amount: v.want_amount || 0,
                created_at: v.created_at || nowSec
            }));
        nostrState.update(s => ({ ...s, pendingAccepts: list }));
    } catch {}
}

function savePendingAccept(swapId, wantAddress, offerAddress, offerMeta = null) {
    try {
        const map = JSON.parse(localStorage.getItem('hemp0x_pending_accepts') || '{}');
        map[swapId] = {
            wantAddress,
            offerAddress,
            offer_chain: offerMeta?.offer_chain || map[swapId]?.offer_chain || 'rvn',
            offer_amount: offerMeta?.offer_amount ?? map[swapId]?.offer_amount ?? 0,
            want_chain: offerMeta?.want_chain || map[swapId]?.want_chain || 'hemp',
            want_amount: offerMeta?.want_amount ?? map[swapId]?.want_amount ?? 0,
            created_at: map[swapId]?.created_at || Math.floor(Date.now() / 1000),
            funded: false
        };
        localStorage.setItem('hemp0x_pending_accepts', JSON.stringify(map));
        refreshPendingAcceptsStore();
    } catch {}
}

function markPendingAcceptFunded(swapId) {
    try {
        const map = JSON.parse(localStorage.getItem('hemp0x_pending_accepts') || '{}');
        if (map[swapId]) {
            map[swapId].funded = true;
            localStorage.setItem('hemp0x_pending_accepts', JSON.stringify(map));
            refreshPendingAcceptsStore();
        }
    } catch {}
}

function getPendingAccept(swapId) {
    try {
        const map = JSON.parse(localStorage.getItem('hemp0x_pending_accepts') || '{}');
        return map[swapId] || null;
    } catch {
        return null;
    }
}

function isWalletLockedError(err) {
    const text = String(err || "").toLowerCase();
    return text.includes("error code: -13")
        || text.includes("walletpassphrase")
        || text.includes("wallet passphrase")
        || text.includes("please enter the wallet passphrase")
        || /wallet.*locked|passphrase|unlock/i.test(text);
}

function notifyUnlockNeeded(err, retryFn) {
    const text = String(err || "");
    const chain = text.toLowerCase().includes("rvn") || text.toLowerCase().includes("ravencoin") ? "rvn" : "hemp";
    window.dispatchEvent(new CustomEvent('hemp0x_swap_unlock_needed', {
        detail: { chain, retryFn, error: text }
    }));
}

function sendRelaySubscriptions(ws) {
    if (!ws || ws.readyState !== WebSocket.OPEN || !pubKeyHex) return;
    const since24h = Math.floor(Date.now() / 1000) - (24 * 60 * 60);
    try {
        ws.send(JSON.stringify([
            "REQ",
            "hemp0x_all",
            {
                kinds: [13337, 13338, 13339, 13340, 13341],
                "#t": ["hemp0x_swap", "hemp0x_swap_handshake", "hemp0x_swap_funded", "hemp0x_swap_counter_funded", "hemp0x_swap_secret"],
                since: since24h
            }
        ]));
        ws.send(JSON.stringify([
            "REQ",
            "hemp0x_dm",
            {
                kinds: [13338, 13339, 13340, 13341],
                "#p": [pubKeyHex],
                since: since24h
            }
        ]));
    } catch {}
}

function updateConnectionStatus() {
    let anyOpen = false;
    for (const ws of relaySockets.values()) {
        if (ws && ws.readyState === WebSocket.OPEN) {
            anyOpen = true;
            break;
        }
    }
    nostrState.update(s => (s.isConnected === anyOpen ? s : { ...s, isConnected: anyOpen }));
}

function connectRelaySocket(url) {
    const existing = relaySockets.get(url);
    if (existing && (existing.readyState === WebSocket.OPEN || existing.readyState === WebSocket.CONNECTING)) {
        return;
    }

    try {
        const ws = new WebSocket(url);
        relaySockets.set(url, ws);

        ws.onopen = () => {
            updateConnectionStatus();
            sendRelaySubscriptions(ws);
        };

        ws.onmessage = (msg) => {
            try {
                const parsed = JSON.parse(msg.data);
                if (!Array.isArray(parsed)) return;
                if (parsed[0] === "EVENT" && parsed[2]) {
                    const event = parsed[2];
                    processGlobalEvent(event);
                    if (
                        event.kind >= 13338 &&
                        event.kind <= 13341 &&
                        event.pubkey !== pubKeyHex
                    ) {
                        processIncomingEvent(event);
                    }
                }
            } catch {}
        };

        ws.onerror = () => {
            try { ws.close(); } catch {}
        };

        ws.onclose = () => {
            if (relaySockets.get(url) === ws) {
                relaySockets.delete(url);
            }
            updateConnectionStatus();
            setTimeout(() => connectRelaySocket(url), 3000);
        };
    } catch {
        setTimeout(() => connectRelaySocket(url), 3000);
    }
}

async function broadcastSignedEvent(signedEvent) {
    let sentViaWs = false;
    const msg = JSON.stringify(["EVENT", signedEvent]);
    for (const ws of relaySockets.values()) {
        if (ws && ws.readyState === WebSocket.OPEN) {
            try {
                ws.send(msg);
                sentViaWs = true;
            } catch {}
        }
    }
    if (pool) {
        try {
            await Promise.any(pool.publish(RELAYS, signedEvent));
            return;
        } catch (e) {
            if (!sentViaWs) throw e;
        }
    }
}

export async function initNostr() {
    loadPersistedSets();
    refreshPendingAcceptsStore();

    if (pollTimer) {
        clearInterval(pollTimer);
        pollTimer = null;
    }

    // 1. Manage Keys
    let storedPrivKey = localStorage.getItem('hemp0x_nostr_privkey');
    if (!storedPrivKey) {
        privKeyBytes = generateSecretKey();
        localStorage.setItem('hemp0x_nostr_privkey', bytesToHex(privKeyBytes));
    } else {
        privKeyBytes = hexToBytes(storedPrivKey);
    }
    pubKeyHex = getPublicKey(privKeyBytes);

    nostrState.update(s => ({ ...s, pubkey: pubKeyHex }));

    // 2. Initialize backup pool and managed persistent WebSocket connections
    if (!pool) {
        pool = new SimplePool({ enablePing: false, enableReconnect: true });
    }

    for (const url of RELAYS) {
        connectRelaySocket(url);
    }

    // 3. Self-healing 6-second subscription refresh across all relays + 12-second active swap reconciliation
    pollTimer = setInterval(async () => {
        pollTicks++;
        for (const url of RELAYS) {
            const ws = relaySockets.get(url);
            if (!ws || ws.readyState === WebSocket.CLOSED) {
                connectRelaySocket(url);
            } else if (ws.readyState === WebSocket.OPEN) {
                sendRelaySubscriptions(ws);
            }
        }
        if (initialRelaySyncDone && pollTicks % 2 === 0) {
            try {
                const localSwaps = await invoke('swap_list');
                await reconcileActiveSwaps(localSwaps);
            } catch {}
        }
    }, 6000);

    // 4. Wait briefly for initial relay history (so closed/accepted swaps are marked first),
    // then sync any still-open local intent offers and reconcile active swaps.
    setTimeout(async () => {
        initialRelaySyncDone = true;
        try {
            const localSwaps = await invoke('swap_list');
            await syncLocalIntentOffers(localSwaps);
        } catch {}
    }, 3500);
}

export async function reconcileActiveSwaps(swaps) {
    if (!initialRelaySyncDone || !Array.isArray(swaps) || !privKeyBytes || isReconcilingSwaps) return;
    isReconcilingSwaps = true;
    try {
        const nowMs = Date.now();
        for (const swap of swaps) {
            if (!swap?.swap_id) continue;

            if (swap.is_initiator) {
                if (swap.phase === 'accepted') {
                    // Initiator finalized counter-HTLC but hasn't claimed/revealed secret yet
                    try {
                        const claimRes = await invoke('swap_claim', {
                            swapId: swap.swap_id,
                            providedSecret: null,
                            peerClaimRawTx: null
                        });
                        const secretMatch = String(claimRes || "").match(/([a-f0-9]{64})$/i);
                        const rawTxMatch = String(claimRes || "").match(/CLAIM_RAW_TX:([a-f0-9]+)/i);
                        const secret = (secretMatch && secretMatch[1]) || swap.secret;
                        const claimRawTx = (rawTxMatch && rawTxMatch[1]) || swap.claim_raw_tx || null;
                        if (secret) {
                            await publishSecretReveal("", "", swap.swap_id, secret, claimRawTx);
                            reconciledClaimSwaps.add(swap.swap_id);
                        }
                        window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
                    } catch {}
                } else if ((swap.phase === 'claimed' || swap.phase === 'completed') && swap.secret && !reconciledClaimSwaps.has(swap.swap_id)) {
                    // Re-broadcast secret + claim_raw_tx once per session so any Acceptor waiting in 'accepted' receives it immediately
                    reconciledClaimSwaps.add(swap.swap_id);
                    let claimRawTx = swap.claim_raw_tx || null;
                    try {
                        const claimRes = await invoke('swap_claim', {
                            swapId: swap.swap_id,
                            providedSecret: null,
                            peerClaimRawTx: null
                        });
                        const rawTxMatch = String(claimRes || "").match(/CLAIM_RAW_TX:([a-f0-9]+)/i);
                        if (rawTxMatch && rawTxMatch[1]) claimRawTx = rawTxMatch[1];
                    } catch {}
                    try {
                        await publishSecretReveal("", "", swap.swap_id, swap.secret, claimRawTx);
                    } catch {}
                }
            } else {
                // Acceptor in 'accepted' phase waiting for Initiator's secret reveal
                if (swap.phase === 'accepted') {
                    const lastSent = lastCounterBroadcastAt.get(swap.swap_id) || 0;
                    if (swap.accept_code && nowMs - lastSent > 25000) {
                        lastCounterBroadcastAt.set(swap.swap_id, nowMs);
                        try {
                            await publishCounterHTLCFunded("", "", swap.accept_code, swap.swap_id);
                        } catch {}
                    }
                    // Also attempt auto-claim (checks mempool on want_chain for revealed preimage!)
                    try {
                        const claimRes = await invoke('swap_claim', {
                            swapId: swap.swap_id,
                            providedSecret: null,
                            peerClaimRawTx: null
                        });
                        const rawTxMatch = String(claimRes || "").match(/CLAIM_RAW_TX:([a-f0-9]+)/i);
                        if (rawTxMatch && rawTxMatch[1]) {
                            await publishAcceptorClaimEcho("", "", swap.swap_id, rawTxMatch[1]);
                        }
                        window.dispatchEvent(new CustomEvent('hemp0x_swap_autoclaimed', { detail: { swap_id: swap.swap_id } }));
                        window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
                    } catch {}
                }
            }
        }
    } finally {
        isReconcilingSwaps = false;
    }
}

export async function syncLocalIntentOffers(swaps) {
    if (!initialRelaySyncDone || !Array.isArray(swaps) || !privKeyBytes) return;
    const currentOffers = get(nostrState).offers;
    for (const swap of swaps) {
        if (!swap?.swap_id) continue;
        if (swap.phase !== 'intent') {
            markSwapClosed(swap.swap_id);
            removeLocalOffer(swap.swap_id);
            continue;
        }
        if (isSwapClosed(swap.swap_id) || syncedIntentSwaps.has(swap.swap_id)) {
            continue;
        }
        if (swap.offer_code) {
            syncedIntentSwaps.add(swap.swap_id);
            const alreadyListed = currentOffers.some(o => o.rawData?.swap_id === swap.swap_id);
            if (!alreadyListed) {
                try {
                    await publishOffer(swap.offer_code);
                } catch {}
            }
        }
    }
    await reconcileActiveSwaps(swaps);
}

export function removeLocalOffer(swapId) {
    if (!swapId) return;
    nostrState.update(s => ({
        ...s,
        offers: s.offers.filter(o => o.rawData?.swap_id !== swapId)
    }));
}

function processGlobalEvent(event) {
    if (!event?.content) return;
    if (event.kind === 13337) {
        try {
            let jsonStr = atob(event.content);
            let data = JSON.parse(jsonStr);

            if (!data.swap_id || !data.offer_chain || !data.want_chain) return;
            if (isSwapClosed(data.swap_id)) return;

            nostrState.update(s => {
                if (isSwapClosed(data.swap_id)) return s;
                if (s.offers.find(o => o.id === event.id || o.rawData?.swap_id === data.swap_id)) return s;

                let newOffer = {
                    id: event.id,
                    pubkey: event.pubkey,
                    created_at: event.created_at,
                    content: event.content,
                    offer_chain: data.offer_chain,
                    want_chain: data.want_chain,
                    offer_amount: data.offer_amount,
                    want_amount: data.want_amount,
                    rawData: data
                };

                if (data.locktime > 0 && Date.now() / 1000 > data.locktime) return s;

                return { ...s, offers: [...s.offers, newOffer].sort((a, b) => b.created_at - a.created_at) };
            });
        } catch {}
    } else if (
        event.kind === 13338 ||
        event.kind === 13339 ||
        event.kind === 13340 ||
        event.kind === 13341
    ) {
        // Because Nostr relays deliver events in reverse-chronological order (newest first),
        // any handshake (13338), HTLC lock (13339/13340), cancellation, or secret reveal (13341)
        // must immediately mark the swap closed so the older 13337 event does not re-add it.
        try {
            let data = JSON.parse(atob(event.content));
            if (data.swap_id) {
                markSwapClosed(data.swap_id);
                removeLocalOffer(data.swap_id);
            }
        } catch {}
    }
}

async function processIncomingEvent(event) {
    if (!event?.id) return;
    if (inFlightEvents.has(event.id) || getProcessedEvents().has(event.id)) return;
    inFlightEvents.add(event.id);

    try {
        let jsonStr = atob(event.content);
        let data = JSON.parse(jsonStr);

        if (event.kind === 13338) {
            // STEP 2: Initiator receives Acceptor's Handshake
            if (!data.swap_id || !data.want_address) return;
            markSwapClosed(data.swap_id);
            removeLocalOffer(data.swap_id);
            try {
                let updatedOfferCode = await invoke('swap_lock_initiator', {
                    swapId: data.swap_id,
                    acceptorWantAddress: data.want_address
                });
                await publishHTLCFunded(event.id, event.pubkey, updatedOfferCode, data.want_address, data.offer_address);
                markEventProcessed(event.id);
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
            } catch (err) {
                const errText = String(err || "");
                if (isWalletLockedError(err)) {
                    notifyUnlockNeeded(err, () => processIncomingEvent(event));
                } else if (errText.includes("not in intent phase") || errText.includes("Swap not found")) {
                    markEventProcessed(event.id);
                } else {
                    console.warn("Failed to lock initiator funds:", err);
                }
            }
        } else if (event.kind === 13339) {
            if (data.cancelled) {
                markSwapClosed(data.swap_id);
                removeLocalOffer(data.swap_id);
                markEventProcessed(event.id);
                return;
            }
            // STEP 3: Acceptor receives Initiator's HTLC Funded
            if (!data.offer_code || !data.swap_id) return;
            markSwapClosed(data.swap_id);
            removeLocalOffer(data.swap_id);

            const pending = getPendingAccept(data.swap_id);
            const isTargetedToMe = Array.isArray(event.tags) && event.tags.some(t => t[0] === "p" && t[1] === pubKeyHex);
            if (!pending && !isTargetedToMe) return;

            const acceptorWantAddress = pending?.wantAddress || data.acceptor_want_address || data.want_address;
            const acceptorOfferAddress = pending?.offerAddress || data.acceptor_offer_address || data.offer_address;
            if (!acceptorWantAddress || !acceptorOfferAddress) return;

            try {
                let acceptCode = await invoke('swap_accept_offer', {
                    offerCode: data.offer_code,
                    wantAddress: acceptorWantAddress,
                    offerAddress: acceptorOfferAddress
                });
                markPendingAcceptFunded(data.swap_id);
                await publishCounterHTLCFunded(event.id, event.pubkey, acceptCode, data.swap_id);
                markEventProcessed(event.id);
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
            } catch (err) {
                if (isWalletLockedError(err)) {
                    notifyUnlockNeeded(err, () => processIncomingEvent(event));
                } else {
                    console.warn("Failed to lock acceptor funds:", err);
                }
            }
        } else if (event.kind === 13340) {
            // STEP 4: Initiator receives Acceptor's Counter-HTLC Funded
            if (!data.swap_id || !data.accept_code) return;
            markSwapClosed(data.swap_id);
            removeLocalOffer(data.swap_id);
            try {
                try {
                    await invoke('swap_finalize_offer', { swapId: data.swap_id, acceptCode: data.accept_code });
                } catch (finErr) {
                    const finText = String(finErr || "");
                    if (finText.includes("Swap not found")) {
                        markEventProcessed(event.id);
                        return;
                    }
                    if (!finText.includes("Swap must be in created phase")) {
                        throw finErr;
                    }
                }
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
                let claimRes = await invoke('swap_claim', {
                    swapId: data.swap_id,
                    providedSecret: null,
                    peerClaimRawTx: null
                });
                
                // Extract secret and signed claim raw tx from claimRes string
                let secretMatch = claimRes.match(/([a-f0-9]{64})$/i);
                let rawTxMatch = claimRes.match(/CLAIM_RAW_TX:([a-f0-9]+)/i);
                if (secretMatch && secretMatch[1]) {
                    await publishSecretReveal(
                        event.id,
                        event.pubkey,
                        data.swap_id,
                        secretMatch[1],
                        rawTxMatch ? rawTxMatch[1] : null
                    );
                }

                markEventProcessed(event.id);
                window.dispatchEvent(new CustomEvent('hemp0x_swap_autoclaimed', { detail: { swap_id: data.swap_id } }));
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
            } catch (err) {
                const errText = String(err || "");
                if (isWalletLockedError(err)) {
                    notifyUnlockNeeded(err, () => processIncomingEvent(event));
                } else if (errText.includes("Swap not found")) {
                    markEventProcessed(event.id);
                } else {
                    console.warn("Failed to finalize and claim:", err);
                }
            }
        } else if (event.kind === 13341) {
            // STEP 5: Acceptor receives Initiator's Secret Reveal (or Initiator receives Acceptor's claim_raw_tx relay)
            if (!data.swap_id) return;
            markSwapClosed(data.swap_id);
            removeLocalOffer(data.swap_id);
            markPendingAcceptFunded(data.swap_id);
            try {
                let claimRes = await invoke('swap_claim', {
                    swapId: data.swap_id,
                    providedSecret: data.secret || null,
                    peerClaimRawTx: data.claim_raw_tx || null
                });
                // If Acceptor signed a claim tx and hasn't echoed it yet, relay claim_raw_tx back so Initiator's synced node broadcasts it too
                if (!data.acceptor_echo) {
                    let rawTxMatch = String(claimRes || "").match(/CLAIM_RAW_TX:([a-f0-9]+)/i);
                    if (rawTxMatch && rawTxMatch[1]) {
                        await publishAcceptorClaimEcho(event.id, event.pubkey, data.swap_id, rawTxMatch[1]);
                    }
                }
                markEventProcessed(event.id);
                window.dispatchEvent(new CustomEvent('hemp0x_swap_autoclaimed', { detail: { swap_id: data.swap_id } }));
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
            } catch (err) {
                const errText = String(err || "");
                if (isWalletLockedError(err)) {
                    notifyUnlockNeeded(err, () => processIncomingEvent(event));
                } else if (errText.includes("Swap not found") || errText.includes("Swap must be in accepted phase")) {
                    markEventProcessed(event.id);
                } else {
                    console.warn("Failed to execute acceptor claim:", err);
                }
            }
        }
    } catch (e) {
        // Invalid content
    } finally {
        inFlightEvents.delete(event.id);
    }
}

export function requestImmediateRelayPoll() {
    for (const url of RELAYS) {
        const ws = relaySockets.get(url);
        if (!ws || ws.readyState === WebSocket.CLOSED) {
            connectRelaySocket(url);
        } else if (ws.readyState === WebSocket.OPEN) {
            sendRelaySubscriptions(ws);
        }
    }
}

export async function publishOffer(offerCodeBase64) {
    if (!privKeyBytes) throw new Error("Nostr not initialized");

    let event = {
        kind: 13337,
        created_at: Math.floor(Date.now() / 1000),
        tags: [
            ["t", "hemp0x_swap"]
        ],
        content: offerCodeBase64
    };

    let signedEvent = finalizeEvent(event, privKeyBytes);

    // Immediately add to local Global Public Offers state
    processGlobalEvent(signedEvent);
    
    // Publish to all relays
    await broadcastSignedEvent(signedEvent);
    return signedEvent.id;
}

export async function publishHandshakeAccept(offerEventId, offerPubkey, swapId, wantAddress, offerAddress, offerMeta = null) {
    if (!privKeyBytes) throw new Error("Nostr not initialized");

    markSwapClosed(swapId);
    removeLocalOffer(swapId);
    savePendingAccept(swapId, wantAddress, offerAddress, offerMeta);

    let payload = btoa(JSON.stringify({
        swap_id: swapId,
        want_address: wantAddress,
        offer_address: offerAddress
    }));

    let event = {
        kind: 13338,
        created_at: Math.floor(Date.now() / 1000),
        tags: [
            ["t", "hemp0x_swap_handshake"],
            ["e", offerEventId],
            ["p", offerPubkey]
        ],
        content: payload
    };

    let signedEvent = finalizeEvent(event, privKeyBytes);
    await broadcastSignedEvent(signedEvent);
    return signedEvent.id;
}

export async function publishHTLCFunded(handshakeEventId, acceptorPubkey, updatedOfferCodeBase64, acceptorWantAddress = null, acceptorOfferAddress = null) {
    if (!privKeyBytes) throw new Error("Nostr not initialized");

    // Extract JSON from the base64 offer code to send structured data
    let offerData = JSON.parse(atob(updatedOfferCodeBase64));
    if (offerData.swap_id) {
        markSwapClosed(offerData.swap_id);
        removeLocalOffer(offerData.swap_id);
    }
    let payload = btoa(JSON.stringify({
        swap_id: offerData.swap_id,
        offer_code: updatedOfferCodeBase64,
        want_address: acceptorWantAddress || offerData.acceptor_want_address || offerData.want_address,
        offer_address: acceptorOfferAddress || offerData.offer_address,
        acceptor_want_address: acceptorWantAddress || offerData.acceptor_want_address || null,
        acceptor_offer_address: acceptorOfferAddress || null
    }));

    let event = {
        kind: 13339,
        created_at: Math.floor(Date.now() / 1000),
        tags: [
            ["t", "hemp0x_swap_funded"],
            ["e", handshakeEventId],
            ["p", acceptorPubkey]
        ],
        content: payload
    };

    let signedEvent = finalizeEvent(event, privKeyBytes);
    await broadcastSignedEvent(signedEvent);
    return signedEvent.id;
}

export async function publishCounterHTLCFunded(htlcFundedEventId, initiatorPubkey, acceptCodeBase64, swapId) {
    if (!privKeyBytes) throw new Error("Nostr not initialized");

    markSwapClosed(swapId);
    removeLocalOffer(swapId);

    let payload = btoa(JSON.stringify({
        swap_id: swapId,
        accept_code: acceptCodeBase64
    }));

    let event = {
        kind: 13340,
        created_at: Math.floor(Date.now() / 1000),
        tags: [
            ["t", "hemp0x_swap_counter_funded"],
            ["e", htlcFundedEventId],
            ["p", initiatorPubkey]
        ],
        content: payload
    };

    let signedEvent = finalizeEvent(event, privKeyBytes);
    await broadcastSignedEvent(signedEvent);
    return signedEvent.id;
}

export async function publishSecretReveal(counterFundedEventId, acceptorPubkey, swapId, secret, claimRawTx = null) {
    if (!privKeyBytes) throw new Error("Nostr not initialized");

    markSwapClosed(swapId);
    removeLocalOffer(swapId);

    let payload = btoa(JSON.stringify({
        swap_id: swapId,
        secret: secret,
        claim_raw_tx: claimRawTx || undefined
    }));

    let event = {
        kind: 13341,
        created_at: Math.floor(Date.now() / 1000),
        tags: [
            ["t", "hemp0x_swap_secret"],
            ["e", counterFundedEventId],
            ["p", acceptorPubkey]
        ],
        content: payload
    };

    let signedEvent = finalizeEvent(event, privKeyBytes);
    await broadcastSignedEvent(signedEvent);
    return signedEvent.id;
}

async function publishAcceptorClaimEcho(secretEventId, initiatorPubkey, swapId, claimRawTx) {
    if (!privKeyBytes || !claimRawTx) return;
    try {
        let payload = btoa(JSON.stringify({
            swap_id: swapId,
            claim_raw_tx: claimRawTx,
            acceptor_echo: true
        }));
        let event = {
            kind: 13341,
            created_at: Math.floor(Date.now() / 1000),
            tags: [
                ["t", "hemp0x_swap_secret"],
                ["e", secretEventId],
                ["p", initiatorPubkey]
            ],
            content: payload
        };
        let signedEvent = finalizeEvent(event, privKeyBytes);
        await broadcastSignedEvent(signedEvent);
    } catch {}
}

export async function publishOfferCancelled(swapId) {
    markSwapClosed(swapId);
    removeLocalOffer(swapId);

    if (!privKeyBytes) return;

    try {
        let payload = btoa(JSON.stringify({
            swap_id: swapId,
            cancelled: true
        }));

        let event = {
            kind: 13339,
            created_at: Math.floor(Date.now() / 1000),
            tags: [
                ["t", "hemp0x_swap_funded"]
            ],
            content: payload
        };

        let signedEvent = finalizeEvent(event, privKeyBytes);
        await broadcastSignedEvent(signedEvent);
    } catch (e) {
        // relay publish best effort
    }
}
