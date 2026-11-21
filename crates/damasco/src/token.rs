//! Random tokens for one-time links and sessions. The plain token travels to
//! the browser once; the database keeps only its SHA-256.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// A new random token: 32 bytes, base64url without padding (43 characters).
pub fn generate() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system random source");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The value stored in the database for a token.
pub fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_unique_and_url_safe() {
        let a = generate();
        let b = generate();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn hash_is_stable() {
        assert_eq!(hash("x"), hash("x"));
        assert_ne!(hash("x"), hash("y"));
        assert_eq!(hash("x").len(), 32);
    }
}
