use ripemd::Ripemd160;
use sha2::{Sha256, Digest};

fn main() {
    let pubkey_hex = "03910e09a89f05e669b5e54b09959f428b8a2b9714768dc5efddfce74044b588b7";
    let pubkey = hex::decode(pubkey_hex).unwrap();
    
    let mut sha256 = Sha256::new();
    sha256.update(&pubkey);
    let hash1 = sha256.finalize();
    
    let mut ripemd = Ripemd160::new();
    ripemd.update(&hash1);
    let hash2 = ripemd.finalize();
    
    println!("Hash160: {}", hex::encode(hash2));
}
