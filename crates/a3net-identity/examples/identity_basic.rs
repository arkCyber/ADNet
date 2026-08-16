//! Tiny example: generate a wallet, sign an arbitrary 32-byte digest,
//! recover the signer via the EIP-191 path, and verify the result.
//!
//! Run with:
//! ```bash
//! cargo run -p a3net-identity --example identity_basic
//! ```

use a3net_identity::{Wallet, WalletPublic};

fn main() {
    // 1. Generate a fresh wallet.
    let wallet = Wallet::generate();
    let addr = wallet.public().address();
    println!("address: {addr}");
    println!("pubkey:  0x{}", wallet.public().public_key_hex());

    // 2. Sign a 32-byte digest. The caller is responsible for the
    //    digest choice (sha256, blake3, keccak256, …); the EIP-191
    //    envelope is fixed across hash choices.
    let digest: [u8; 32] = blake3::hash(b"hello a3net").into();
    let sig = wallet.sign_personal(&digest).expect("sign");
    println!("sig r: 0x{}", hex::encode(sig.r));
    println!("sig s: 0x{}", hex::encode(sig.s));
    println!("sig v: {}", sig.v);

    // 3. Recover the signer from the digest + signature.
    let recovered = WalletPublic::recover_personal(&digest, &sig).expect("recover");
    println!("recovered: {}", recovered.address());
    assert_eq!(recovered.address(), addr);

    // 4. Round-trip the public key bytes.
    let pk = wallet.public().public_key_bytes();
    let same = WalletPublic::from_compressed(&pk).expect("from_compressed");
    assert_eq!(same.address(), addr);
    println!("pubkey round-trip: ok");
}
