import { SimplePool } from 'nostr-tools';
let pool = new SimplePool();
let sub = pool.subscribeMany([], [], { onevent: () => {} });
console.log(Object.keys(sub));
console.log(typeof sub.onevent);
