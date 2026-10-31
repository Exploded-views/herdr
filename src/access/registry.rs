// herdr-fork: the named-caller registry: tokens stored only as sha256, each bound to one pane's access binding. Unofficial stopgap; see FORK.md.
//! Named callers for `access.caller.*`. A caller's token is shown once, when it
//! is minted; only its sha256 is kept, in memory and in the registry file.
//! Nothing here prints, logs or formats a token.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::policy::{valid_caller_name, AccessBindingId};

type TokenHash = [u8; 32];

/// Where the registry lives: next to the session's other state.
pub(crate) fn registry_path() -> PathBuf {
    crate::session::data_dir().join("access-callers.json")
}

#[derive(Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    binding: AccessBindingId,
    hash: TokenHash,
}

#[derive(Clone, PartialEq, Eq, Default)]
pub(crate) struct CallerRegistry {
    entries: Vec<Entry>,
}

// Not derived: keep even the hashes out of any log line.
impl std::fmt::Debug for CallerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallerRegistry")
            .field("callers", &self.entries.len())
            .finish()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct FileFormat {
    callers: Vec<FileEntry>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct FileEntry {
    name: String,
    binding: AccessBindingId,
    token_sha256: String,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn hash_token(token: &str) -> TokenHash {
    Sha256::digest(token.as_bytes()).into()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn unhex_hash(text: &str) -> Option<TokenHash> {
    if text.len() != 64 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Compare two hashes without stopping at the first difference.
fn ct_eq(a: &TokenHash, b: &TokenHash) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

fn mint_token() -> io::Result<String> {
    Ok(hex(&crate::platform::random_secret()?))
}

impl CallerRegistry {
    /// A missing file is an empty registry; an unreadable or malformed one is
    /// an error (its text is never put in the message).
    pub(crate) fn load(path: &Path) -> io::Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        let file: FileFormat = serde_json::from_str(&text)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "malformed caller registry"))?;
        let mut registry = Self::default();
        for entry in file.callers {
            let hash = unhex_hash(&entry.token_sha256).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "malformed caller registry entry",
                )
            })?;
            if !valid_caller_name(&entry.name) || registry.position(&entry.name).is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "malformed caller registry entry",
                ));
            }
            registry.entries.push(Entry {
                name: entry.name,
                binding: entry.binding,
                hash,
            });
        }
        Ok(registry)
    }

    /// Write to a 0600 temp file in the same directory, then rename over `path`.
    pub(crate) fn save(&self, path: &Path) -> io::Result<()> {
        let file = FileFormat {
            callers: self
                .entries
                .iter()
                .map(|entry| FileEntry {
                    name: entry.name.clone(),
                    binding: entry.binding.clone(),
                    token_sha256: hex(&entry.hash),
                })
                .collect(),
        };
        let body = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
        if let Some(dir) = dir {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        // A stale temp from a crashed run is ours to clear.
        let _ = std::fs::remove_file(&tmp);
        let result = (|| {
            let mut out = crate::platform::create_private_file(&tmp)?;
            out.write_all(&body)?;
            out.sync_all()?;
            drop(out);
            std::fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|entry| entry.name == name)
    }

    /// Register `name` for `target` and return its token. The token is not
    /// kept; only its sha256 is.
    pub(crate) fn add(&mut self, name: &str, target: AccessBindingId) -> io::Result<String> {
        if !valid_caller_name(name) {
            return Err(invalid("caller name must match [a-z0-9][a-z0-9_-]{0,63}"));
        }
        if self.position(name).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("caller {name} already exists"),
            ));
        }
        let token = mint_token()?;
        self.entries.push(Entry {
            name: name.to_string(),
            binding: target,
            hash: hash_token(&token),
        });
        Ok(token)
    }

    /// Replace `name`'s token; the old one stops working.
    pub(crate) fn rotate(&mut self, name: &str) -> io::Result<String> {
        let index = self.position(name).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("no caller named {name}"))
        })?;
        let token = mint_token()?;
        self.entries[index].hash = hash_token(&token);
        Ok(token)
    }

    pub(crate) fn retarget(&mut self, name: &str, target: AccessBindingId) -> Result<(), String> {
        let index = self
            .position(name)
            .ok_or_else(|| format!("no caller named {name}"))?;
        self.entries[index].binding = target;
        Ok(())
    }

    pub(crate) fn remove(&mut self, name: &str) -> bool {
        match self.position(name) {
            Some(index) => {
                self.entries.remove(index);
                true
            }
            None => false,
        }
    }

    /// The caller a token belongs to. Every entry is compared, so the time
    /// taken does not depend on which entry matches or where.
    pub(crate) fn lookup(&self, token: &str) -> Option<(String, AccessBindingId)> {
        let presented = hash_token(token);
        let mut found: Option<&Entry> = None;
        for entry in &self.entries {
            if ct_eq(&entry.hash, &presented) {
                found = Some(entry);
            }
        }
        found.map(|entry| (entry.name.clone(), entry.binding.clone()))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Name and binding of every caller, in registration order.
    pub(crate) fn callers(&self) -> impl Iterator<Item = (&str, &AccessBindingId)> {
        self.entries
            .iter()
            .map(|entry| (entry.name.as_str(), &entry.binding))
    }
}

/// A scratch directory under the system temp dir, removed on drop. Tests
/// point the registry at one so they never touch the real data dir.
#[cfg(test)]
pub(crate) struct TestDir(PathBuf);

#[cfg(test)]
impl TestDir {
    pub(crate) fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("herdr-callers-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bid(n: u8) -> AccessBindingId {
        AccessBindingId::parse(&format!("acc_{n:032x}")).unwrap()
    }

    fn temp_dir() -> TestDir {
        TestDir::new("registry")
    }

    #[test]
    fn add_returns_token_and_lookup_matches() {
        let mut registry = CallerRegistry::default();
        assert!(registry.is_empty());
        let token = registry.add("alice", bid(1)).unwrap();
        assert!(!registry.is_empty());
        assert_eq!(registry.lookup(&token), Some(("alice".to_string(), bid(1))));
        let other = registry.add("bob", bid(2)).unwrap();
        assert_eq!(registry.lookup(&other), Some(("bob".to_string(), bid(2))));
        assert_eq!(registry.lookup(&token), Some(("alice".to_string(), bid(1))));
    }

    #[test]
    fn lookup_wrong_token_is_none() {
        let mut registry = CallerRegistry::default();
        let token = registry.add("alice", bid(1)).unwrap();
        assert_eq!(registry.lookup(&"0".repeat(64)), None);
        assert_eq!(registry.lookup(""), None);
        assert_eq!(registry.lookup(&token[..63]), None);
        assert_eq!(registry.lookup(&format!("{token}0")), None);
        assert_eq!(CallerRegistry::default().lookup(&token), None);
    }

    #[test]
    fn rotate_invalidates_old_token() {
        let mut registry = CallerRegistry::default();
        let old = registry.add("alice", bid(1)).unwrap();
        let new = registry.rotate("alice").unwrap();
        assert_ne!(old, new);
        assert_eq!(registry.lookup(&old), None);
        assert_eq!(registry.lookup(&new), Some(("alice".to_string(), bid(1))));
        assert!(registry.rotate("nobody").is_err());
    }

    #[test]
    fn token_is_64_hex_and_unique() {
        let mut registry = CallerRegistry::default();
        let mut seen = std::collections::HashSet::new();
        for i in 0..32 {
            let token = registry.add(&format!("c{i}"), bid(1)).unwrap();
            assert_eq!(token.len(), 64);
            assert!(token
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
            assert!(seen.insert(token));
        }
    }

    #[test]
    fn save_load_roundtrip_mode_0600() {
        let dir = temp_dir();
        let path = dir.path().join("access-callers.json");
        let mut registry = CallerRegistry::default();
        let token = registry.add("alice", bid(1)).unwrap();
        registry.save(&path).unwrap();
        let loaded = CallerRegistry::load(&path).unwrap();
        assert_eq!(loaded, registry);
        assert_eq!(loaded.lookup(&token), Some(("alice".to_string(), bid(1))));
        // Only the hash is on disk.
        assert!(!std::fs::read_to_string(&path).unwrap().contains(&token));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A second save replaces the file and leaves no temp behind.
        registry.save(&path).unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn load_missing_is_empty_and_malformed_is_an_error_without_content() {
        let dir = temp_dir();
        let path = dir.path().join("access-callers.json");
        assert!(CallerRegistry::load(&path).unwrap().is_empty());
        std::fs::write(&path, "{\"callers\": SECRET-TEXT").unwrap();
        let err = CallerRegistry::load(&path).unwrap_err();
        assert!(!err.to_string().contains("SECRET-TEXT"));
        std::fs::write(
            &path,
            format!(
                "{{\"callers\":[{{\"name\":\"Bad Name\",\"binding\":\"{}\",\"token_sha256\":\"{}\"}}]}}",
                bid(1),
                "0".repeat(64)
            ),
        )
        .unwrap();
        assert!(CallerRegistry::load(&path).is_err());
    }

    #[test]
    fn names_are_validated_and_unique() {
        let mut registry = CallerRegistry::default();
        for bad in ["", "Alice", "-a", "a b", &"a".repeat(65)] {
            assert!(registry.add(bad, bid(1)).is_err(), "{bad:?}");
        }
        registry.add("alice", bid(1)).unwrap();
        assert!(registry.add("alice", bid(2)).is_err());
        assert!(registry.add(&"a".repeat(64), bid(1)).is_ok());
    }

    #[test]
    fn retarget_and_remove() {
        let mut registry = CallerRegistry::default();
        let token = registry.add("alice", bid(1)).unwrap();
        registry.retarget("alice", bid(2)).unwrap();
        assert_eq!(registry.lookup(&token), Some(("alice".to_string(), bid(2))));
        assert!(registry.retarget("nobody", bid(2)).is_err());
        assert!(registry.remove("alice"));
        assert!(!registry.remove("alice"));
        assert_eq!(registry.lookup(&token), None);
    }

    #[test]
    fn debug_shows_no_token_or_hash() {
        let mut registry = CallerRegistry::default();
        let token = registry.add("alice", bid(1)).unwrap();
        let text = format!("{registry:?}");
        assert!(!text.contains(&token));
        assert!(!text.contains(&hex(&hash_token(&token))));
    }
}
