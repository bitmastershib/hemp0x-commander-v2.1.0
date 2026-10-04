import { SimplePool, generateSecretKey, getPublicKey, finalizeEvent } from 'nostr-tools';

async function test() {
    try {
        console.log("Imported SimplePool:", !!SimplePool);
        let pool = new SimplePool();
        console.log("Created pool");
        
        let privKeyBytes = generateSecretKey();
        
        let event = {
            kind: 1,
            created_at: Math.floor(Date.now() / 1000),
            tags: [
                ["t", "hemp0x_test_event"]
            ],
            content: "Test event"
        };
        
        let signedEvent = finalizeEvent(event, privKeyBytes);
        console.log("Publishing to damus...");
        let pub = pool.publish(['wss://relay.damus.io'], signedEvent);
        await Promise.any(pub);
        console.log("Published kind 1!");

        let event2 = {
            kind: 13337,
            created_at: Math.floor(Date.now() / 1000),
            tags: [
                ["t", "hemp0x_test_event"]
            ],
            content: "Test event 2"
        };
        let signedEvent2 = finalizeEvent(event2, privKeyBytes);
        console.log("Publishing kind 13337...");
        let pub2 = pool.publish(['wss://relay.damus.io'], signedEvent2);
        await Promise.any(pub2);
        console.log("Published kind 13337!");
        
        pool.close(['wss://relay.damus.io']);
    } catch(e) {
        console.error("Error:", e);
    }
}
test();
