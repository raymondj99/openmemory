//! SQLCipher key application (lawyerBrain `v0.4.4-lb1` patch).
//!
//! When a connection is opened with a cipher key, the key MUST be applied as the **first**
//! operation on that connection (before any other PRAGMA or query) so SQLCipher can decrypt
//! page 1. This helper centralises that. It is a **no-op when `key` is `None`**, so a SQLCipher
//! build with no key behaves exactly like stock SQLite (plaintext) — keeping every existing
//! caller and test green. `key` is the raw 32-byte AES-256 data key.

use rusqlite::Connection;

/// Apply the raw `key` to `conn` (no-op if `None`). Call immediately after `Connection::open`.
pub fn apply_cipher_key(conn: &Connection, key: Option<&[u8]>) -> rusqlite::Result<()> {
    if let Some(key) = key {
        let mut hex = String::with_capacity(key.len() * 2);
        for b in key {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
        }
        // Raw-key form (x'..' skips the KDF); pin the SQLCipher 4 page format.
        conn.execute_batch(&format!(
            "PRAGMA key = \"x'{hex}'\"; PRAGMA cipher_compatibility = 4;"
        ))?;
    }
    Ok(())
}
