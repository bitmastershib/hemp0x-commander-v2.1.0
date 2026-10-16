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
    'wss://relay.damus.io',
    'wss://nos.lol',
    'wss://relay.primal.net',
    'wss://offchain.pub',
    'wss://nostr.mom'
];

export const nostrState = writable({
    isConnected: false,
    pubkey: null,
    offers: [] // Array of { id, pubkey, created_at, content, offer_chain, want_chain, offer_amount, want_amount, rawData }
});

let pool;
let privKeyBytes;
let pubKeyHex;
let subOffers;
let subAccepts;
const inFlightEvents = new Set();

function getProcessedEvents() {
    try {
        return new Set(JSON.parse(localStorage.getItem('hemp0x_nostr_processed_events') || '[]'));
    } catch {
        return new Set();
    }
}

function markEventProcessed(eventId) {
    try {
        const set = getProcessedEvents();
        set.add(eventId);
        const arr = Array.from(set).slice(-500);
        localStorage.setItem('hemp0x_nostr_processed_events', JSON.stringify(arr));
    } catch {}
}

function savePendingAccept(swapId, wantAddress, offerAddress) {
    try {
        const map = JSON.parse(localStorage.getItem('hemp0x_pending_accepts') || '{}');
        map[swapId] = { wantAddress, offerAddress };
        localStorage.setItem('hemp0x_pending_accepts', JSON.stringify(map));
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

export async function initNostr() {
    if (subOffers) {
        try { subOffers.close(); } catch {}
    }
    if (subAccepts) {
        try { subAccepts.close(); } catch {}
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

    // 2. Connect to Relays
    pool = new SimplePool({ enablePing: true, enableReconnect: true });
    
    nostrState.update(s => ({ ...s, isConnected: true }));

    // 3. Subscribe to the Marketplace (Offers and Accepts globally)
    let since24h = Math.floor(Date.now() / 1000) - (24 * 60 * 60);
    
    subOffers = pool.subscribeMany(
        RELAYS,
        {
            kinds: [13337, 13338, 13339, 13340],
            "#t": ["hemp0x_swap", "hemp0x_swap_handshake", "hemp0x_swap_funded", "hemp0x_swap_counter_funded"],
            since: since24h
        },
        {
            onevent(event) {
                processGlobalEvent(event);
            }
        }
    );

    // 4. Subscribe to Incoming Direct Messages targeting our pubkey
    subAccepts = pool.subscribeMany(
        RELAYS,
        {
            kinds: [13338, 13339, 13340, 13341],
            "#p": [pubKeyHex],
            since: since24h
        },
        {
            onevent(event) {
                processIncomingEvent(event);
            }
        }
    );

    // 5. Re-broadcast and display any active local intent offers
    try {
        const localSwaps = await invoke('swap_list');
        await syncLocalIntentOffers(localSwaps);
    } catch (e) {
        // Ignore startup swap_list errors
    }
}

export async function syncLocalIntentOffers(swaps) {
    if (!Array.isArray(swaps) || !privKeyBytes || !pool) return;
    const currentOffers = get(nostrState).offers;
    for (const swap of swaps) {
        if (swap.phase === 'intent' && swap.offer_code) {
            const alreadyListed = currentOffers.some(o => o.rawData?.swap_id === swap.swap_id);
            if (!alreadyListed) {
                try {
                    await publishOffer(swap.offer_code);
                } catch (e) {
                    // Local offer is still added by publishOffer before relay broadcast
                }
            }
        }
    }
}

export function removeLocalOffer(swapId) {
    nostrState.update(s => ({
        ...s,
        offers: s.offers.filter(o => o.rawData?.swap_id !== swapId)
    }));
}

function processGlobalEvent(event) {
    if (event.kind === 13337) {
        try {
            let jsonStr = atob(event.content);
            let data = JSON.parse(jsonStr);

            if (!data.swap_id || !data.offer_chain || !data.want_chain) return;

            nostrState.update(s => {
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
        } catch (e) {}
    } else if (event.kind === 13339 || event.kind === 13340) {
        // Remove an offer from the marketplace once it is funded
        try {
            let data = JSON.parse(atob(event.content));
            if (data.swap_id) {
                removeLocalOffer(data.swap_id);
            }
        } catch (e) {}
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
            try {
                let updatedOfferCode = await invoke('swap_lock_initiator', {
                    swapId: data.swap_id,
                    acceptorWantAddress: data.want_address
                });
                await publishHTLCFunded(event.id, event.pubkey, updatedOfferCode, data.want_address, data.offer_address);
                markEventProcessed(event.id);
                removeLocalOffer(data.swap_id);
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
            } catch (err) {
                if (isWalletLockedError(err)) {
                    notifyUnlockNeeded(err, () => processIncomingEvent(event));
                } else {
                    console.warn("Failed to lock initiator funds:", err);
                }
            }
        } else if (event.kind === 13339) {
            // STEP 3: Acceptor receives Initiator's HTLC Funded
            if (!data.offer_code || !data.swap_id) return;
            const pending = getPendingAccept(data.swap_id);
            const acceptorWantAddress = pending?.wantAddress || data.acceptor_want_address || data.want_address;
            const acceptorOfferAddress = pending?.offerAddress || data.acceptor_offer_address || data.offer_address;
            if (!acceptorWantAddress || !acceptorOfferAddress) return;

            try {
                let acceptCode = await invoke('swap_accept_offer', {
                    offerCode: data.offer_code,
                    wantAddress: acceptorWantAddress,
                    offerAddress: acceptorOfferAddress
                });
                await publishCounterHTLCFunded(event.id, event.pubkey, acceptCode, data.swap_id);
                markEventProcessed(event.id);
                removeLocalOffer(data.swap_id);
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
            try {
                await invoke('swap_finalize_offer', { swapId: data.swap_id, acceptCode: data.accept_code });
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
                let claimRes = await invoke('swap_claim', { swapId: data.swap_id, providedSecret: null });
                
                // Extract secret from claimRes string
                let secretMatch = claimRes.match(/([a-f0-9]{64})$/i);
                if (secretMatch && secretMatch[1]) {
                    await publishSecretReveal(event.id, event.pubkey, data.swap_id, secretMatch[1]);
                }

                markEventProcessed(event.id);
                window.dispatchEvent(new CustomEvent('hemp0x_swap_autoclaimed', { detail: { swap_id: data.swap_id } }));
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
            } catch (err) {
                if (isWalletLockedError(err)) {
                    notifyUnlockNeeded(err, () => processIncomingEvent(event));
                } else {
                    console.warn("Failed to finalize and claim:", err);
                }
            }
        } else if (event.kind === 13341) {
            // STEP 5: Acceptor receives Initiator's Secret Reveal
            if (!data.swap_id || !data.secret) return;
            try {
                await invoke('swap_claim', { swapId: data.swap_id, providedSecret: data.secret });
                markEventProcessed(event.id);
                window.dispatchEvent(new CustomEvent('hemp0x_swap_autoclaimed', { detail: { swap_id: data.swap_id } }));
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
            } catch (err) {
                if (isWalletLockedError(err)) {
                    notifyUnlockNeeded(err, () => processIncomingEvent(event));
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

export async function publishOffer(offerCodeBase64) {
    if (!pool || !privKeyBytes) throw new Error("Nostr not initialized");

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
    await Promise.any(pool.publish(RELAYS, signedEvent));
    return signedEvent.id;
}

export async function publishHandshakeAccept(offerEventId, offerPubkey, swapId, wantAddress, offerAddress) {
    if (!pool || !privKeyBytes) throw new Error("Nostr not initialized");

    savePendingAccept(swapId, wantAddress, offerAddress);

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
    await Promise.any(pool.publish(RELAYS, signedEvent));
    return signedEvent.id;
}

export async function publishHTLCFunded(handshakeEventId, acceptorPubkey, updatedOfferCodeBase64, acceptorWantAddress = null, acceptorOfferAddress = null) {
    if (!pool || !privKeyBytes) throw new Error("Nostr not initialized");

    // Extract JSON from the base64 offer code to send structured data
    let offerData = JSON.parse(atob(updatedOfferCodeBase64));
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
    await Promise.any(pool.publish(RELAYS, signedEvent));
    return signedEvent.id;
}

export async function publishCounterHTLCFunded(htlcFundedEventId, initiatorPubkey, acceptCodeBase64, swapId) {
    if (!pool || !privKeyBytes) throw new Error("Nostr not initialized");

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
    await Promise.any(pool.publish(RELAYS, signedEvent));
    return signedEvent.id;
}

export async function publishSecretReveal(counterFundedEventId, acceptorPubkey, swapId, secret) {
    if (!pool || !privKeyBytes) throw new Error("Nostr not initialized");

    let payload = btoa(JSON.stringify({
        swap_id: swapId,
        secret: secret
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
    await Promise.any(pool.publish(RELAYS, signedEvent));
    return signedEvent.id;
}
