//! Small shared helpers: hashing, IDs, tokens and timestamps.

use sha2::{Digest, Sha256};

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Random hex string from the OS CSPRNG. Used for tokens and record IDs.
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("OS random number generator unavailable");
    hex::encode(buf)
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", random_hex(8))
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Stable file ID derived from a repository path.
pub fn file_id(path: &str) -> String {
    format!("f_{}", &sha256_hex(path.as_bytes())[..16])
}

/// Constant-time comparison for secrets.
pub fn secure_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_ids_are_stable_and_distinct() {
        assert_eq!(file_id("src/a.rs"), file_id("src/a.rs"));
        assert_ne!(file_id("src/a.rs"), file_id("src/b.rs"));
    }

    #[test]
    fn random_hex_has_expected_length() {
        assert_eq!(random_hex(16).len(), 32);
        assert_ne!(random_hex(16), random_hex(16));
    }

    #[test]
    fn secure_eq_compares() {
        assert!(secure_eq("abc", "abc"));
        assert!(!secure_eq("abc", "abd"));
        assert!(!secure_eq("abc", "abcd"));
    }
}
