//! MIoT signature algorithm implementation
//!
//! Implements nonce/signedNonce/signature generation required by the Xiaomi MIoT cloud API.

use crate::error::{Result, SmartHomeError};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

pub struct MiotCrypto;

impl MiotCrypto {
    pub fn new() -> Self {
        Self
    }

    /// nonce = base64(random(8) + timestamp(4 bytes, minute resolution))
    pub fn generate_nonce(&self) -> String {
        let mut buf = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut buf[..8]);

        let minutes = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() / 60;

        buf[8] = ((minutes >> 24) & 0xff) as u8;
        buf[9] = ((minutes >> 16) & 0xff) as u8;
        buf[10] = ((minutes >> 8) & 0xff) as u8;
        buf[11] = (minutes & 0xff) as u8;

        BASE64.encode(&buf)
    }

    /// signedNonce = base64( SHA256( base64Dec(ssecurity) ++ base64Dec(nonce) ) )
    pub fn generate_signed_nonce(&self, ssecurity: &str, nonce: &str) -> Result<String> {
        let secret = BASE64.decode(ssecurity)
            .map_err(|e| SmartHomeError::Signature(format!("bad ssecurity: {}", e)))?;
        let nonce_bytes = BASE64.decode(nonce)
            .map_err(|e| SmartHomeError::Signature(format!("bad nonce: {}", e)))?;

        let mut hasher = Sha256::new();
        hasher.update(&secret);
        hasher.update(&nonce_bytes);
        Ok(BASE64.encode(&hasher.finalize()))
    }

    /// signString = "<uri>&<signedNonce>&<nonce>&data=<data>"
    /// signature  = base64( HMAC-SHA256(key=base64Dec(signedNonce), msg=signString) )
    pub fn generate_signature(
        &self,
        uri: &str,
        signed_nonce: &str,
        nonce: &str,
        data: &str,
    ) -> Result<String> {
        let sign_string = format!("{}&{}&{}&data={}", uri, signed_nonce, nonce, data);
        let key = BASE64.decode(signed_nonce)
            .map_err(|e| SmartHomeError::Signature(format!("bad signedNonce: {}", e)))?;

        let mut mac = HmacSha256::new_from_slice(&key)
            .map_err(|e| SmartHomeError::Signature(format!("HMAC init: {}", e)))?;
        mac.update(sign_string.as_bytes());
        Ok(BASE64.encode(&mac.finalize().into_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_is_12_bytes() {
        let c = MiotCrypto::new();
        let nonce = c.generate_nonce();
        let decoded = BASE64.decode(&nonce).unwrap();
        assert_eq!(decoded.len(), 12);
    }

    #[test]
    fn signed_nonce_is_32_bytes() {
        let c = MiotCrypto::new();
        let ssecurity = BASE64.encode(b"0123456789abcdef0123456789abcdef");
        let nonce = c.generate_nonce();
        let signed = c.generate_signed_nonce(&ssecurity, &nonce).unwrap();
        let decoded = BASE64.decode(&signed).unwrap();
        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn signature_is_non_empty() {
        let c = MiotCrypto::new();
        let ssecurity = BASE64.encode(b"0123456789abcdef0123456789abcdef");
        let nonce = c.generate_nonce();
        let signed = c.generate_signed_nonce(&ssecurity, &nonce).unwrap();
        let sig = c.generate_signature("/home/device_list", &signed, &nonce, "{}").unwrap();
        assert!(!sig.is_empty());
    }
}
