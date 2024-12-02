use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Performs a timing-safe comparison of two strings
pub fn secure_compare(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut result = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        result |= x ^ y;
    }
    result == 0
}

/// A token manager that can sign and verify data using HMAC-SHA256
pub struct Token {
    key: String,
    secret: String,
}

impl Token {
    /// Creates a new Token instance with the given key and secret
    ///
    /// # Arguments
    ///
    /// * `key` - The application key
    /// * `secret` - The application secret used for signing
    pub fn new(key: String, secret: String) -> Self {
        Token { key, secret }
    }

    /// Signs the input string using HMAC-SHA256 with the secret
    ///
    /// # Arguments
    ///
    /// * `input` - The string to be signed
    ///
    /// # Returns
    ///
    /// Returns the hexadecimal representation of the HMAC signature
    pub fn sign(&self, input: String) -> String {
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes())
            .expect("HMAC can take key of any size");

        mac.update(input.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    /// Verifies if the provided signature matches the computed signature for the input
    ///
    /// # Arguments
    ///
    /// * `input` - The original input string
    /// * `signature` - The signature to verify against
    ///
    /// # Returns
    ///
    /// Returns true if the signatures match, false otherwise
    pub fn verify(&self, input: String, signature: &str) -> bool {
        secure_compare(&self.sign(input), signature)
    }
}
