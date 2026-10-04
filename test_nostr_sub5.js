import { SimplePool, generateSecretKey, getPublicKey, finalizeEvent } from 'nostr-tools';

async function test() {
    try {
        let pool = new SimplePool();
        let privKeyBytes = generateSecretKey();
        
        let event = {
            kind: 13337,
            created_at: Math.floor(Date.now() / 1000),
            tags: [
                ["t", "hemp0x_swap"]
            ],
            content: "eyJzd2FwX2lkIjoic29tZS1pZCJ9" // base64 for {"swap_id":"some-id"}
        };
        let signedEvent = finalizeEvent(event, privKeyBytes);
        
        let sub = pool.subscribeMany(
            ['wss://nos.lol', 'wss://relay.primal.net', 'wss://relay.damus.io'],
            {
                kinds: [13337],
                "#t": ["hemp0x_swap"],
            },
            {
                onevent(event) {
                    console.log("Received event!", event.kind);
                }
            }
        );

        console.log("Publishing...");
        let pub = pool.publish(['wss://nos.lol', 'wss://relay.primal.net', 'wss://relay.damus.io'], signedEvent);
        await Promise.any(pub);
        console.log("Published!");

        setTimeout(() => {
            console.log("Timeout");
            pool.close(['wss://nos.lol', 'wss://relay.primal.net', 'wss://relay.damus.io']);
        }, 5000);
    } catch(e) {
        console.error("Error:", e);
    }
}
test();
