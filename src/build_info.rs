// herdr-fork: adds `fork_tag`, `version_line` (switch `VERSION_LINE_NAMES_FORK`), the git commit, the binary path and its lazy hash, and `server_identity`, which the server reports. Unofficial stopgap; see FORK.md.
//! Build identity helpers.

pub const BASE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn channel() -> &'static str {
    non_empty(option_env!("HERDR_BUILD_CHANNEL")).unwrap_or("stable")
}

pub fn build_id() -> Option<&'static str> {
    non_empty(option_env!("HERDR_BUILD_ID"))
}

pub fn version() -> String {
    match channel() {
        "stable" => BASE_VERSION.to_string(),
        channel => match build_id() {
            Some(build_id) => format!("{BASE_VERSION}-{channel}.{build_id}"),
            None => format!("{BASE_VERSION}-{channel}"),
        },
    }
}

/// The fork release tag baked in at build time (`HERDR_FORK_TAG`, e.g. `v0.9.5-ev.1`).
pub fn fork_tag() -> Option<&'static str> {
    non_empty(option_env!("HERDR_FORK_TAG"))
}

/// The text `--version` prints. For now that is upstream's `herdr 0.9.5`, exactly (see
/// `VERSION_LINE_NAMES_FORK`); the fork's identity is in `herdr status`. `version()` is
/// unchanged: it feeds the handoff and API version comparisons.
pub fn version_line() -> String {
    version_line_for(&version(), fork_tag())
}

/// The one switch for the fork tag in `--version`. `false` keeps `--version` identical to
/// upstream (`herdr 0.9.5`); the fork identity is in `herdr status`. Flip to `true` once crewkit
/// accepts `herdr X.Y.Z (fork vTAG)`.
const VERSION_LINE_NAMES_FORK: bool = false;

fn version_line_for(version: &str, tag: Option<&str>) -> String {
    if !VERSION_LINE_NAMES_FORK {
        return format!("herdr {version}");
    }
    fork_version_line_for(version, tag)
}

/// The deferred format: `herdr 0.9.5 (fork v0.9.5-ev.N)`, or `(fork, untagged build)`.
fn fork_version_line_for(version: &str, tag: Option<&str>) -> String {
    match tag {
        Some(tag) => format!("herdr {version} (fork {tag})"),
        None => format!("herdr {version} (fork, untagged build)"),
    }
}

/// The git commit the build was made from (`build.rs` bakes it in); `None` when it had no git.
pub fn commit() -> Option<&'static str> {
    non_empty(option_env!("HERDR_FORK_COMMIT"))
}

/// What this process tells `ping` callers about the build it runs.
pub fn server_identity() -> crate::api::schema::ServerIdentity {
    identity_for(
        BINARY_PATH.get().cloned().or_else(|| {
            resolve_binary_path()
                .ok()
                .map(|path| path.display().to_string())
        }),
        fork_tag(),
        commit(),
        binary_sha256(),
    )
}

fn identity_for(
    binary: Option<String>,
    fork_tag: Option<&str>,
    commit: Option<&str>,
    binary_sha256: Option<String>,
) -> crate::api::schema::ServerIdentity {
    crate::api::schema::ServerIdentity {
        binary,
        fork_tag: fork_tag.map(str::to_owned),
        commit: commit.map(str::to_owned),
        binary_sha256,
    }
}

/// The sha256 of a file, as lowercase hex. It reads the file in blocks, so a large binary
/// is never held in memory.
pub fn hash_file(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut block = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut block)?;
        if read == 0 {
            break;
        }
        hasher.update(&block[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// A path with symlinks resolved. On macOS `current_exe()` returns the symlink the process was
/// started through, which can later point at a different file.
pub fn resolve(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    std::fs::canonicalize(path)
}

fn resolve_binary_path() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe().and_then(|path| resolve(&path))
}

/// The resolved path of this executable, fixed when the server started.
static BINARY_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static BINARY_SHA256: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static BINARY_HASH_STARTED: std::sync::Once = std::sync::Once::new();

/// Fixes this executable's resolved path. A server calls it at start: one `current_exe` and
/// `canonicalize`, no file reads. Hashing waits for the first identity request.
pub fn on_server_start() {
    let _ = remember_binary_path();
}

fn remember_binary_path() -> Option<std::path::PathBuf> {
    match resolve_binary_path() {
        Ok(path) => {
            let _ = BINARY_PATH.set(path.display().to_string());
            // The first resolved path wins, so the hash reads the file `binary` names.
            BINARY_PATH.get().map(std::path::PathBuf::from)
        }
        Err(error) => {
            tracing::warn!(%error, "could not resolve the server binary path");
            None
        }
    }
}

/// Starts hashing this executable on a background thread, at most once per process, so neither
/// the accept thread nor the app loop waits on it. The first caller starts it; later calls do
/// nothing. If the file cannot be read the hash stays unset and status reports null.
fn start_binary_hash() {
    BINARY_HASH_STARTED.call_once(|| {
        let Some(path) = remember_binary_path() else {
            return;
        };
        HASH_RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let spawned =
            crate::thread_spawn::spawn_named("herdr-binary-hash", move || match hash_file(&path) {
                Ok(hash) => {
                    let _ = BINARY_SHA256.set(hash);
                }
                Err(error) => tracing::warn!(%error, "could not hash the server binary"),
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start the binary hash thread");
        }
    });
}

static HASH_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many times this process has started hashing its executable (a test hook).
#[cfg(test)]
pub fn hash_runs() -> usize {
    HASH_RUNS.load(std::sync::atomic::Ordering::SeqCst)
}

/// The hash of this executable, cached for the life of the process. The first call starts the
/// hash in the background and returns `None`; calls return `None` until it has finished.
pub fn binary_sha256() -> Option<String> {
    if let Some(hash) = BINARY_SHA256.get() {
        return Some(hash.clone());
    }
    start_binary_hash();
    BINARY_SHA256.get().cloned()
}

pub fn is_preview() -> bool {
    channel() == "preview"
}

fn non_empty(value: Option<&'static str>) -> Option<&'static str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn stable_version_defaults_to_cargo_version() {
        assert!(!super::version().is_empty());
    }

    #[test]
    fn the_version_line_is_upstream_identical() {
        assert_eq!(
            super::version_line_for("0.9.5", Some("v0.9.5-ev.3")),
            "herdr 0.9.5"
        );
        assert_eq!(super::version_line_for("0.9.5", None), "herdr 0.9.5");
    }

    #[test]
    fn the_server_identity_carries_the_fork_tag() {
        let identity = super::identity_for(
            Some("/bin/herdr".into()),
            Some("v0.9.5-ev.3"),
            Some("abc"),
            Some("def".into()),
        );
        assert_eq!(identity.fork_tag.as_deref(), Some("v0.9.5-ev.3"));
        assert_eq!(identity.commit.as_deref(), Some("abc"));
        assert_eq!(identity.binary_sha256.as_deref(), Some("def"));
        assert_eq!(identity.binary.as_deref(), Some("/bin/herdr"));
    }

    #[test]
    fn the_version_line_names_the_fork_tag() {
        assert_eq!(
            super::fork_version_line_for("0.9.5", Some("v0.9.5-ev.1")),
            "herdr 0.9.5 (fork v0.9.5-ev.1)"
        );
    }

    #[test]
    fn an_untagged_build_still_says_fork() {
        assert_eq!(
            super::fork_version_line_for("0.9.5", None),
            "herdr 0.9.5 (fork, untagged build)"
        );
    }

    #[test]
    fn the_hash_of_the_test_binary_matches_its_sha256() {
        use sha2::{Digest, Sha256};
        let exe = std::env::current_exe().unwrap();
        let bytes = std::fs::read(&exe).unwrap();
        let want: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(want.len(), 64);
        assert_eq!(super::hash_file(&exe).unwrap(), want);
    }

    #[test]
    fn the_hash_of_a_known_file_is_its_sha256() {
        let path = std::env::temp_dir().join(format!("herdr-hash-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        let got = super::hash_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            got,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn wait_for_hash() -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(hash) = super::server_identity().binary_sha256 {
                return hash;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "hash never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn starting_a_server_does_not_start_hashing() {
        super::on_server_start();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(super::hash_runs(), 0);
        assert_eq!(super::BINARY_SHA256.get(), None);
    }

    #[test]
    fn the_first_identity_request_hashes_once_and_later_ones_reuse_it() {
        super::on_server_start();
        assert_eq!(super::hash_runs(), 0);
        let first = wait_for_hash();
        assert_eq!(super::hash_runs(), 1);
        for _ in 0..5 {
            assert_eq!(super::server_identity().binary_sha256, Some(first.clone()));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(super::hash_runs(), 1);
    }

    #[test]
    fn concurrent_first_requests_start_one_hash() {
        let threads: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(super::server_identity))
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        wait_for_hash();
        assert_eq!(super::hash_runs(), 1);
    }

    #[test]
    fn the_cached_hash_is_the_sha256_of_the_test_binary() {
        use sha2::{Digest, Sha256};
        let bytes = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        let want: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(wait_for_hash(), want);
    }

    #[test]
    fn resolve_follows_a_symlink_to_the_file() {
        let dir = std::env::temp_dir().join(format!("herdr-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("real");
        let link = dir.join("link");
        std::fs::write(&file, b"x").unwrap();
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let resolved = super::resolve(&link).unwrap();
        let want = std::fs::canonicalize(&file).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(resolved, want);
        assert_ne!(resolved, link);
    }

    #[test]
    fn the_identity_names_this_executable_and_the_build() {
        let identity = super::server_identity();
        assert_eq!(
            identity.binary,
            Some(
                std::fs::canonicalize(std::env::current_exe().unwrap())
                    .unwrap()
                    .display()
                    .to_string()
            )
        );
        assert_eq!(identity.fork_tag.as_deref(), super::fork_tag());
        assert_eq!(identity.commit.as_deref(), super::commit());
    }

    #[test]
    fn a_build_made_from_git_bakes_in_the_commit() {
        // Skips when the tree has no git (a source tarball): then the build has no commit.
        let head = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output();
        let Ok(head) = head else { return };
        if !head.status.success() {
            assert_eq!(super::commit(), None);
            return;
        }
        let commit = super::commit().expect("a git checkout bakes its commit into the build");
        assert_eq!(commit.len(), 40);
        assert!(commit.bytes().all(|b| b.is_ascii_hexdigit()));
    }
}
