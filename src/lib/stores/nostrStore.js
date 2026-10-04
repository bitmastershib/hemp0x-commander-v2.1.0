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
    'wss://relay.primal.net'
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

export async function initNostr() {
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
    pool = new SimplePool();
    
    // We can consider connected if at least one relay works, but SimplePool abstracts this.
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
}

function processGlobalEvent(event) {
    if (event.kind === 13337) {
        try {
            let jsonStr = atob(event.content);
            let data = JSON.parse(jsonStr);

            if (!data.swap_id || !data.offer_chain || !data.want_chain) return;

            nostrState.update(s => {
                if (s.offers.find(o => o.id === event.id || o.rawData.swap_id === data.swap_id)) return s;

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
    } else if (event.kind === 13338 || event.kind === 13339 || event.kind === 13340) {
        // Someone accepted or funded an offer. Remove it from the marketplace!
        let offerEventIdTag = event.tags.find(t => t[0] === 'e');
        if (offerEventIdTag) {
            let offerIdToRemove = offerEventIdTag[1];
            nostrState.update(s => ({
                ...s,
                offers: s.offers.filter(o => o.id !== offerIdToRemove)
            }));
        }
    }
}

async function processIncomingEvent(event) {
    try {
        let jsonStr = atob(event.content);
        let data = JSON.parse(jsonStr);

        if (event.kind === 13338) {
            // STEP 2: Initiator receives Acceptor's Handshake
            if (!data.swap_id || !data.want_address) return;
            console.log("Phase 3: Received Handshake for swap", data.swap_id);
            try {
                let updatedOfferCode = await invoke('swap_lock_initiator', { swap_id: data.swap_id, acceptor_want_address: data.want_address });
                console.log("Phase 3: Initiator locked funds. Publishing 13339...");
                await publishHTLCFunded(event.id, event.pubkey, updatedOfferCode);
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
            } catch (err) {
                console.warn("Failed to lock initiator funds:", err);
            }
        } else if (event.kind === 13339) {
            // STEP 3: Acceptor receives Initiator's HTLC Funded
            if (!data.offer_code || !data.offer_address || !data.want_address) return;
            console.log("Phase 3: Received HTLC Funded for swap. Acceptor is locking...");
            try {
                let acceptCode = await invoke('swap_accept_offer', {
                    offerCode: data.offer_code,
                    wantAddress: data.want_address,
                    offerAddress: data.offer_address
                });
                console.log("Phase 3: Acceptor locked funds. Publishing 13340...");
                await publishCounterHTLCFunded(event.id, event.pubkey, acceptCode, data.swap_id);
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
            } catch (err) {
                console.warn("Failed to lock acceptor funds:", err);
            }
        } else if (event.kind === 13340) {
            // STEP 4: Initiator receives Acceptor's Counter-HTLC Funded
            if (!data.swap_id || !data.accept_code) return;
            console.log("Phase 3: Received Counter-HTLC Funded for swap", data.swap_id);
            try {
                await invoke('swap_finalize_offer', { swapId: data.swap_id, acceptCode: data.accept_code });
                let claimRes = await invoke('swap_claim', { swapId: data.swap_id, providedSecret: null });
                console.log("Phase 3: Initiator Claimed!", claimRes);
                
                // Extract secret from claimRes string (e.g. "... funds: <secret_hex>")
                let secretMatch = claimRes.match(/([a-f0-9]{64})$/i);
                if (secretMatch && secretMatch[1]) {
                    console.log("Phase 3: Extracted secret. Broadcasting 13341...");
                    await publishSecretReveal(event.id, event.pubkey, data.swap_id, secretMatch[1]);
                }

                window.dispatchEvent(new CustomEvent('hemp0x_swap_autoclaimed', { detail: { swap_id: data.swap_id } }));
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
            } catch (err) {
                console.warn("Failed to finalize and claim:", err);
            }
        } else if (event.kind === 13341) {
            // STEP 5: Acceptor receives Initiator's Secret Reveal
            if (!data.swap_id || !data.secret) return;
            console.log("Phase 3: Received Secret Reveal for swap", data.swap_id);
            try {
                await invoke('swap_claim', { swapId: data.swap_id, providedSecret: data.secret });
                console.log("Phase 3: Acceptor Claimed!");
                window.dispatchEvent(new CustomEvent('hemp0x_swap_autoclaimed', { detail: { swap_id: data.swap_id } }));
                window.dispatchEvent(new CustomEvent('hemp0x_swap_updated'));
            } catch (err) {
                console.warn("Failed to execute acceptor claim:", err);
            }
        }
    } catch (e) {
        // Invalid content
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
    
    // Publish to all relays
    await Promise.any(pool.publish(RELAYS, signedEvent));
    return signedEvent.id;
}

export async function publishHandshakeAccept(offerEventId, offerPubkey, swapId, wantAddress, offerAddress) {
    if (!pool || !privKeyBytes) throw new Error("Nostr not initialized");

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

export async function publishHTLCFunded(handshakeEventId, acceptorPubkey, updatedOfferCodeBase64) {
    if (!pool || !privKeyBytes) throw new Error("Nostr not initialized");

    // Extract JSON from the base64 offer code to send structured data
    let offerData = JSON.parse(atob(updatedOfferCodeBase64));
    let payload = btoa(JSON.stringify({
        swap_id: offerData.swap_id,
        offer_code: updatedOfferCodeBase64,
        want_address: offerData.want_address,
        offer_address: offerData.offer_address
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
