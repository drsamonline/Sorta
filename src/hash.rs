//! Content hashing and the fast-rejection filter.
//!
//! Duplicate detection runs on every incoming file, so the common case — "this
//! is new" — has to be cheap. The path is:
//!
//! 1. 4 KB pre-hash, looked up in a small map. Most files die here.
//! 2. Bloom filter over full BLAKE3 digests. A miss is definitive: new file.
//! 3. Full BLAKE3 streaming hash, then an exact registry lookup.
//!
//! The Bloom filter can only produce false *positives*, which cost one extra
//! hash comparison. It never produces a false negative, so it can never hide a
//! duplicate.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A BLAKE3 digest rendered as lowercase hex, prefixed `b3:`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub String);

impl ContentHash {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Streaming BLAKE3 over the whole file. Never loads the file into memory.
pub fn hash_file(path: &Path) -> io::Result<ContentHash> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
            }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(ContentHash(format!("b3:{}", hasher.finalize().to_hex())))
}

/// Hash of the first 4 KB only, for cheap rejection.
pub fn pre_hash(path: &Path) -> io::Result<u64> {
    let head = crate::detect::read_head(path, 4096)?;
    let digest = blake3::hash(&head);
    let bytes = digest.as_bytes();
    let mut out = [0u8; 8];
    out.copy_from_slice(&bytes[..8]);
    Ok(u64::from_le_bytes(out))
}

/// A Bloom filter sized for a known expected population.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BloomFilter {
    bits: Vec<u64>,
    bit_count: usize,
    hash_count: u32,
    inserted: usize,
}

impl BloomFilter {
    /// Sizes the filter for `expected` items at roughly `fp_rate` false positives.
    pub fn new(expected: usize, fp_rate: f64) -> Self {
        let expected = expected.max(16);
        let fp = fp_rate.clamp(1e-6, 0.5);
        let ln2 = std::f64::consts::LN_2;
        let m = (-(expected as f64) * fp.ln() / (ln2 * ln2)).ceil() as usize;
        let k = ((m as f64 / expected as f64) * ln2).round().max(1.0) as u32;
        let words = m.div_ceil(64);
        Self {
            bits: vec![0u64; words.max(1)],
            bit_count: m.max(64),
            hash_count: k,
            inserted: 0,
        }
    }

    /// Convenience default: 1% false positives.
    pub fn with_capacity(expected: usize) -> Self {
        Self::new(expected, 0.01)
    }

    pub fn len(&self) -> usize {
        self.inserted
    }

    pub fn is_empty(&self) -> bool {
        self.inserted == 0
    }

    fn positions(&self, item: &str) -> Vec<usize> {
        // Two independent 64-bit hashes, combined by double hashing:
        // h_i = h1 + i * h2 (mod m). Standard, and cheap.
        let h1 = fnv1a64(item.as_bytes(), 0xcbf2_9ce4_8422_2325);
        let h2 = fnv1a64(item.as_bytes(), 0x9e37_79b9_7f4a_7c15) | 1;
        (0..self.hash_count)
            .map(|i| {
                ((h1.wrapping_add((i as u64).wrapping_mul(h2))) % self.bit_count as u64) as usize
            })
            .collect()
    }

    /// Adds an item. Returns true if it was probably already present.
    pub fn insert(&mut self, item: &str) -> bool {
        let positions = self.positions(item);
        let mut seen = true;
        for pos in &positions {
            let word = pos / 64;
            let bit = 1u64 << (pos % 64);
            if self.bits[word] & bit == 0 {
                seen = false;
                self.bits[word] |= bit;
            }
        }
        if !seen {
            self.inserted += 1;
        }
        seen
    }

    /// Tests membership. False means definitely absent.
    pub fn contains(&self, item: &str) -> bool {
        self.positions(item).into_iter().all(|pos| {
            let word = pos / 64;
            let bit = 1u64 << (pos % 64);
            self.bits[word] & bit != 0
        })
    }
}

fn fnv1a64(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = seed;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Two-stage duplicate detector: pre-hash map + Bloom + exact registry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FastRejector {
    pre_hashes: BTreeMap<u64, Vec<ContentHash>>,
    #[serde(default)]
    bloom: Option<BloomFilter>,
}

impl FastRejector {
    pub fn new(expected: usize) -> Self {
        Self {
            pre_hashes: BTreeMap::new(),
            bloom: Some(BloomFilter::with_capacity(expected)),
        }
    }

    /// Records a known file.
    pub fn record(&mut self, pre: u64, hash: &ContentHash) {
        self.pre_hashes.entry(pre).or_default().push(hash.clone());
        if let Some(bloom) = self.bloom.as_mut() {
            bloom.insert(hash.as_str());
        }
    }

    /// Cheap check: could this file be a duplicate?
    ///
    /// Returns the candidate hashes to compare exactly. An empty result means
    /// "definitely new, do not bother hashing".
    pub fn candidates(&self, pre: u64) -> &[ContentHash] {
        match self.pre_hashes.get(&pre) {
            Some(v) => v.as_slice(),
            None => &[],
        }
    }

    /// Bloom-only check on a full digest.
    pub fn might_contain(&self, hash: &ContentHash) -> bool {
        match self.bloom.as_ref() {
            Some(bloom) => bloom.contains(hash.as_str()),
            None => true,
        }
    }

    pub fn len(&self) -> usize {
        self.pre_hashes.values().map(|v| v.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.pre_hashes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_stable_and_prefixed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, b"hello").unwrap();
        let h1 = hash_file(&path).unwrap();
        let h2 = hash_file(&path).unwrap();
        assert_eq!(h1, h2);
        assert!(h1.as_str().starts_with("b3:"));
        assert_eq!(h1.as_str().len(), 3 + 64);
    }

    #[test]
    fn different_content_different_hash() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, b"one").unwrap();
        std::fs::write(&b, b"two").unwrap();
        assert_ne!(hash_file(&a).unwrap(), hash_file(&b).unwrap());
    }

    #[test]
    fn bloom_has_no_false_negatives() {
        let mut bloom = BloomFilter::with_capacity(1000);
        for i in 0..500 {
            bloom.insert(&format!("b3:{i}"));
        }
        for i in 0..500 {
            assert!(bloom.contains(&format!("b3:{i}")), "missed {i}");
        }
        assert!(!bloom.contains("b3:definitely-not-there"));
    }

    #[test]
    fn fast_rejector_rejects_new_files() {
        let mut fast = FastRejector::new(100);
        fast.record(42, &ContentHash("b3:abc".to_string()));
        assert_eq!(fast.candidates(42).len(), 1);
        assert!(fast.candidates(43).is_empty());
        assert!(fast.might_contain(&ContentHash("b3:abc".to_string())));
    }
}
