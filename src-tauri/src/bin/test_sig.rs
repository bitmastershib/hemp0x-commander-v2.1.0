use k256::ecdsa::{SigningKey, Signature};

fn main() {
    let secret = vec![1u8; 32];
    let signing_key = SigningKey::from_slice(&secret).unwrap();
    let sighash = vec![2u8; 32];
    
    let (sig, _) = signing_key.sign_prehash_recoverable(&sighash).unwrap();
    // How to normalize S on RecoverableSignature?
    // Let's just use it and convert to DER.
    let mut der = sig.to_der().as_bytes().to_vec();
    println!("DER length: {}", der.len());
    
    // Is there a normalize_s on Signature?
    // Let's check.
    let standard_sig: Signature = sig.into();
    let normalized = standard_sig.normalize_s().unwrap_or(standard_sig);
    let mut final_der = normalized.to_der().as_bytes().to_vec();
    println!("Normalized DER length: {}", final_der.len());
}
