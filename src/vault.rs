//! The encrypted vault.
//!
//! Files marked sensitive are stored as AES-256-GCM ciphertext instead of
//! plaintext. This module is the cryptography and the file format; the CLI wraps
//! it as `sorta vault`.
//!
//! # What this protects, and what it does not
//!
//! It protects the *contents* of a locked file: a backup that copies the folder, a
//! sync client, a colleague with read access, `scp`, a stolen disk image of the data
//! partition. It does **not** protect against someone who has your user account,
//! because the key lives on the same machine.
//!
//! The spec called for the key in the OS keychain (`ring` for the crypto). Neither
//! is here, for two stated reasons:
//!
//! * `ring` ships C and assembly; `aes-gcm` (RustCrypto) is pure Rust and keeps the
//!   no-C-toolchain property the rest of the crate has.
//! * `keyring` needs a Secret Service on Linux, which a headless or minimal install
//!   does not have. A key file with `0600` permissions works everywhere and fails
//!   loudly; a keychain that is absent fails mysteriously. `SORTA_VAULT_KEY` (hex)
//!   overrides both for CI and for users who keep keys elsewhere.
//!
//! # File format
//!
//! ```text
//! "SORV" | version(1) | nonce(12) | AES-256-GCM ciphertext + tag
//! ```
//!
//! The header exists so a file can be recognised as locked without the key.

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key, Nonce};
use serde::{Deserialize, Serialize};

/// Magic bytes at the start of a locked file.
pub const MAGIC: &[u8; 4] = b"SORV";
/// Format version.
pub const VERSION: u8 = 1;
/// Length of the GCM nonce.
pub const NONCE_LEN: usize = 12;
/// Length of an AES-256 key.
pub const KEY_LEN: usize = 32;
/// Environment variable that overrides the key file.
pub const KEY_ENV: &str = "SORTA_VAULT_KEY";
/// Key file name inside the state directory.
pub const KEY_FILE: &str = "vault.key";
/// Folder, under the destination root, where locked files live.
pub const VAULT_DIR: &str = "_Sensitive";
/// Suffix for a locked copy.
pub const LOCKED_SUFFIX: &str = ".sortvault";

/// What a key file looks like on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyFile {
    version: u8,
    /// Hex, so the file is inspectable (and recognisable) without a binary editor.
    key: String,
    created_at: String,
}

/// A 256-bit key.
pub struct VaultKey {
    bytes: [u8; KEY_LEN],
}

impl std::fmt::Debug for VaultKey {
    /// Prints the fingerprint, never the key: a `{:?}` in a log must not be a leak.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VaultKey({})", self.fingerprint())
    }
}

impl VaultKey {
    /// Reads the key from `SORTA_VAULT_KEY`, else from the key file.
    ///
    /// Never creates one: `sorta vault init` does that, deliberately.
    pub fn load(state_dir: &Path) -> anyhow::Result<Self> {
        if let Some(hex) = std::env::var(KEY_ENV).ok().filter(|v| !v.trim().is_empty()) {
            return Self::from_hex(hex.trim())
                .ok_or_else(|| anyhow::anyhow!("{KEY_ENV} is not 64 hex characters"));
        }

        let path = state_dir.join(KEY_FILE);
        let text = std::fs::read_to_string(&path).map_err(|_| {
            anyhow::anyhow!(
                "no vault key at {}; run `sorta vault init` first",
                path.display()
            )
        })?;
        let file: KeyFile = serde_json::from_str(&text)?;
        Self::from_hex(&file.key)
            .ok_or_else(|| anyhow::anyhow!("{} does not contain a valid key", path.display()))
    }

    /// Creates the key file if it is missing, and returns the key.
    ///
    /// The file is written `0600` on Unix, and the call fails rather than silently
    /// writing a world-readable key.
    pub fn load_or_create(state_dir: &Path) -> anyhow::Result<(Self, bool)> {
        let path = state_dir.join(KEY_FILE);
        if path.exists() {
            return Ok((Self::load(state_dir)?, false));
        }

        let mut bytes = [0u8; KEY_LEN];
        use aes_gcm::aead::rand_core::RngCore;
        OsRng.fill_bytes(&mut bytes);
        let key = Self { bytes };

        crate::paths::ensure_dir(state_dir)?;
        let file = KeyFile {
            version: VERSION,
            key: key.to_hex(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&file)?)?;
        restrict(&path)?;
        Ok((key, true))
    }

    /// Parses 64 hex characters.
    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != KEY_LEN * 2 {
            return None;
        }
        let mut bytes = [0u8; KEY_LEN];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(Self { bytes })
    }

    /// The key as hex. Only for display by `vault init`, never logged.
    pub fn to_hex(&self) -> String {
        self.bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// A short, non-secret identifier so two keys can be told apart.
    pub fn fingerprint(&self) -> String {
        let digest = blake3::hash(&self.bytes);
        digest.to_hex().chars().take(8).collect()
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.bytes))
    }
}

#[cfg(unix)]
fn restrict(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> anyhow::Result<()> {
    // Windows inherits the user profile ACL, which is already user-only.
    Ok(())
}

/// Encrypts a file into `out`. Returns the ciphertext length.
pub fn encrypt_file(key: &VaultKey, source: &Path, out: &Path) -> anyhow::Result<u64> {
    let plaintext = std::fs::read(source)?;
    let cipher = key.cipher();
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_slice())
        .map_err(|_| anyhow::anyhow!("encryption failed"))?;

    let mut payload = Vec::with_capacity(MAGIC.len() + 1 + NONCE_LEN + ciphertext.len());
    payload.extend_from_slice(MAGIC);
    payload.push(VERSION);
    payload.extend_from_slice(nonce.as_slice());
    payload.extend_from_slice(&ciphertext);

    if let Some(parent) = out.parent() {
        crate::paths::ensure_dir(parent)?;
    }
    std::fs::write(out, &payload)?;
    Ok(payload.len() as u64)
}

/// Decrypts a locked file into `out`.
pub fn decrypt_file(key: &VaultKey, locked: &Path, out: &Path) -> anyhow::Result<()> {
    let payload = std::fs::read(locked)?;
    let plaintext = decrypt_bytes(key, &payload)?;

    if let Some(parent) = out.parent() {
        crate::paths::ensure_dir(parent)?;
    }
    std::fs::write(out, plaintext)?;
    Ok(())
}

/// Decrypts an in-memory payload, after checking the header.
pub fn decrypt_bytes(key: &VaultKey, payload: &[u8]) -> anyhow::Result<Vec<u8>> {
    let header = MAGIC.len() + 1;
    if payload.len() < header + NONCE_LEN || !payload.starts_with(MAGIC) {
        anyhow::bail!("not a locked Sorta file");
    }
    if payload[MAGIC.len()] != VERSION {
        anyhow::bail!("unsupported vault version {}", payload[MAGIC.len()]);
    }

    let nonce = Nonce::from_slice(&payload[header..header + NONCE_LEN]);
    key.cipher()
        .decrypt(nonce, &payload[header + NONCE_LEN..])
        .map_err(|_| anyhow::anyhow!("decryption failed: wrong key, or the file was altered"))
}

/// True when a file starts with the vault magic bytes.
pub fn is_locked(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 4];
    matches!(file.read_exact(&mut head), Ok(()) if &head == MAGIC)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(byte: &str) -> String {
        byte.repeat(KEY_LEN)
    }

    fn key() -> VaultKey {
        VaultKey::from_hex(&hex("ab")).expect("valid hex")
    }

    #[test]
    fn round_trips_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("secret.pdf");
        let locked = dir.path().join("secret.pdf" + LOCKED_SUFFIX);
        let restored = dir.path().join("restored.pdf");
        std::fs::write(&source, b"salary spreadsheet, do not share").unwrap();

        let key = key();
        let size = encrypt_file(&key, &source, &locked).unwrap();
        assert!(size > 0);
        assert!(is_locked(&locked));
        assert!(!is_locked(&source));

        // The plaintext must not appear in the ciphertext.
        let raw = std::fs::read(&locked).unwrap();
        assert!(!raw.windows(5).any(|w| w == b"salary"));

        decrypt_file(&key, &locked, &restored).unwrap();
        assert_eq!(
            std::fs::read(&restored).unwrap(),
            b"salary spreadsheet, do not share"
        );
    }

    #[test]
    fn the_wrong_key_fails_loudly() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.txt");
        let locked = dir.path().join("a.txt" + LOCKED_SUFFIX);
        std::fs::write(&source, b"hello").unwrap();

        encrypt_file(&key(), &source, &locked).unwrap();
        let other = VaultKey::from_hex(&hex("cd")).unwrap();
        let error = decrypt_file(&other, &locked, &dir.path().join("out.txt")).unwrap_err();
        assert!(error.to_string().contains("wrong key"));
    }

    #[test]
    fn tampering_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.txt");
        let locked = dir.path().join("a.txt" + LOCKED_SUFFIX);
        std::fs::write(&source, b"hello world").unwrap();
        encrypt_file(&key(), &source, &locked).unwrap();

        let mut payload = std::fs::read(&locked).unwrap();
        let last = payload.len() - 1;
        payload[last] ^= 0xff;
        std::fs::write(&locked, &payload).unwrap();

        assert!(decrypt_file(&key(), &locked, &dir.path().join("out")).is_err());
    }

    #[test]
    fn a_plain_file_is_not_mistaken_for_a_locked_one() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain.txt");
        std::fs::write(&plain, b"just text").unwrap();
        assert!(!is_locked(&plain));
        assert!(decrypt_file(&key(), &plain, &dir.path().join("out")).is_err());
    }

    #[test]
    fn hex_parsing_is_strict() {
        assert!(VaultKey::from_hex(&hex("ab")).is_some());
        assert!(VaultKey::from_hex("abcd").is_none());
        assert!(VaultKey::from_hex(&hex("zz")).is_none());
    }

    #[test]
    fn a_key_file_is_created_once_and_restricted() {
        let dir = tempfile::tempdir().unwrap();
        let (first, created) = VaultKey::load_or_create(dir.path()).unwrap();
        assert!(created);
        assert_eq!(first.to_hex().len(), 64);
        assert_eq!(first.fingerprint().len(), 8);

        let (second, created_again) = VaultKey::load_or_create(dir.path()).unwrap();
        assert!(!created_again, "the existing key must be reused");
        assert_eq!(first.to_hex(), second.to_hex());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the key file must be user-only");
        }
    }

    #[test]
    fn loading_without_a_key_says_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let error = VaultKey::load(dir.path()).unwrap_err();
        assert!(error.to_string().contains("vault init"));
    }
}
