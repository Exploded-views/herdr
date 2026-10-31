// herdr-fork: where the CLI finds its caller token file (`--caller-token-file` or HERDR_CALLER_TOKEN_FILE) and how it reads it. Unofficial stopgap; see FORK.md.
//! The token is read for the local server only and is never printed: a
//! failure names the file and the OS error, not what the file held.

use std::ffi::OsString;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::api::schema::CallerToken;

pub(crate) const ENV_VAR: &str = "HERDR_CALLER_TOKEN_FILE";
const FLAG: &str = "--caller-token-file";
/// A token is 64 hex digits; anything near this size is not one.
const MAX_FILE_BYTES: u64 = 1024;

static FLAG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Strip a leading `--caller-token-file <path>` and remember the path.
pub(crate) fn extract_flag(args: &[String]) -> Result<Vec<String>, String> {
    let (rest, path) = split_flag(args)?;
    if let Some(path) = path {
        // Set once, at startup; a second call (tests aside) keeps the first.
        let _ = FLAG_PATH.set(path);
    }
    Ok(rest)
}

/// `args` without a leading `--caller-token-file`, and the path it named.
/// Only the leading options are looked at; after the command the flag is the
/// command's own argument.
fn split_flag(args: &[String]) -> Result<(Vec<String>, Option<PathBuf>), String> {
    let mut rest = Vec::with_capacity(args.len());
    rest.extend(args.first().cloned());
    let mut found: Option<PathBuf> = None;
    let mut index = 1;
    while let Some(arg) = args.get(index) {
        if arg == "--" || !arg.starts_with('-') {
            break;
        }
        let value = if arg == FLAG {
            index += 1;
            Some(
                args.get(index)
                    .cloned()
                    .ok_or("missing value for --caller-token-file")?,
            )
        } else {
            arg.strip_prefix("--caller-token-file=").map(str::to_owned)
        };
        match value {
            Some(value) => {
                if value.is_empty() || value.starts_with('-') {
                    return Err("--caller-token-file needs a path".into());
                }
                if found.replace(PathBuf::from(value)).is_some() {
                    return Err("--caller-token-file can only be specified once".into());
                }
            }
            None => {
                rest.push(arg.clone());
                if matches!(
                    arg.as_str(),
                    "--session" | "--machine" | "--remote" | "--remote-keybindings"
                ) {
                    index += 1;
                    rest.extend(args.get(index).cloned());
                }
            }
        }
        index += 1;
    }
    rest.extend(args.get(index..).unwrap_or_default().iter().cloned());
    Ok((rest, found))
}

fn read_token(path: &Path) -> Result<CallerToken, String> {
    let unreadable = |why: &dyn std::fmt::Display| {
        format!("caller token file {} unreadable: {why}", path.display())
    };
    let file = std::fs::File::open(path).map_err(|err| unreadable(&err))?;
    let mut body = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|err| unreadable(&err))?;
    if body.len() as u64 > MAX_FILE_BYTES {
        return Err(unreadable(&"file is larger than 1024 bytes"));
    }
    let text = String::from_utf8(body).map_err(|_| unreadable(&"not valid UTF-8"))?;
    let token = text.trim();
    if token.is_empty() {
        return Err(unreadable(&"file is empty"));
    }
    if token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(unreadable(&"not a single-line token"));
    }
    Ok(CallerToken::new(token.to_owned()))
}

/// The token to send: the flag's file, else the environment's, else none.
/// The flag wins; a bad environment value is not read when it is given.
pub(crate) fn resolve(
    flag: Option<&Path>,
    env: Option<OsString>,
) -> Result<Option<CallerToken>, String> {
    let path = match (flag, env) {
        (Some(path), _) => path.to_path_buf(),
        (None, Some(value)) if !value.is_empty() => PathBuf::from(value),
        _ => return Ok(None),
    };
    read_token(&path).map(Some)
}

/// The caller token file could not be used. It travels inside an `io::Error`
/// to `finish_cli`, which prints it as a usage error and exits 2.
#[derive(Debug)]
struct UnreadableTokenFile(String);

impl std::fmt::Display for UnreadableTokenFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UnreadableTokenFile {}

/// Wrap a `resolve` failure message for the trip to `finish_cli`.
pub(crate) fn unreadable_error(message: String) -> io::Error {
    io::Error::other(UnreadableTokenFile(message))
}

/// The line `finish_cli` prints (and exits 2 on) when `err` is an unusable token file.
pub(crate) fn usage_report(err: &io::Error) -> Option<String> {
    err.get_ref()
        .and_then(|inner| inner.downcast_ref::<UnreadableTokenFile>())
        .map(|unreadable| format!("error: {}", unreadable.0))
}

/// `resolve` with this process's flag and environment.
pub(crate) fn for_local() -> Result<Option<CallerToken>, String> {
    #[cfg(test)]
    if let Some(path) = TEST_FLAG.with(|flag| flag.borrow().clone()) {
        return resolve(Some(&path), None);
    }
    resolve(
        FLAG_PATH.get().map(PathBuf::as_path),
        std::env::var_os(ENV_VAR),
    )
}

#[cfg(test)]
thread_local! {
    /// Stands in for the `--caller-token-file` flag in tests, per thread.
    static TEST_FLAG: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Point `for_local` at `path` (as the flag would) until the returned guard drops.
#[cfg(test)]
pub(crate) fn set_test_flag(path: &Path) -> impl Drop {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_FLAG.with(|flag| *flag.borrow_mut() = None);
        }
    }
    TEST_FLAG.with(|flag| *flag.borrow_mut() = Some(path.to_path_buf()));
    Reset
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::registry::TestDir;

    const TOKEN: &str = "ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12";

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn token_file(dir: &TestDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn caller_token_file_unreadable_is_usage_error() {
        let dir = TestDir::new("token-unreadable");
        let missing = dir.path().join("missing");
        let message = resolve(Some(&missing), None).unwrap_err();
        assert!(
            message.starts_with(&format!(
                "caller token file {} unreadable: ",
                missing.display()
            )),
            "{message}"
        );
        // A directory, an empty file and an oversized file are unreadable too.
        let empty = token_file(&dir, "empty", "\n");
        assert!(resolve(Some(&empty), None)
            .unwrap_err()
            .contains("unreadable"));
        let huge = token_file(&dir, "huge", &"a".repeat(5000));
        assert!(resolve(Some(&huge), None)
            .unwrap_err()
            .contains("unreadable"));
        assert!(resolve(Some(dir.path()), None)
            .unwrap_err()
            .contains("unreadable"));
        // The environment's file fails the same way.
        let message = resolve(None, Some(missing.clone().into_os_string())).unwrap_err();
        assert!(message.contains("unreadable"), "{message}");
        // The text of a bad file is never echoed.
        let bad = token_file(&dir, "bad", "secret words here");
        let message = resolve(Some(&bad), None).unwrap_err();
        assert!(!message.contains("secret"), "{message}");
    }

    #[test]
    fn token_is_read_and_trimmed_and_the_flag_wins_over_the_environment() {
        let dir = TestDir::new("token-read");
        let good = token_file(&dir, "good", &format!("{TOKEN}\n"));
        let other = token_file(&dir, "other", "ffff\n");
        let missing = dir.path().join("missing");
        let token = resolve(Some(&good), None).unwrap().unwrap();
        assert_eq!(token.expose(), TOKEN);
        let from_env = resolve(None, Some(other.clone().into_os_string()))
            .unwrap()
            .unwrap();
        assert_eq!(from_env.expose(), "ffff");
        // The flag wins; a bad environment value is not even read.
        let token = resolve(Some(&good), Some(missing.into_os_string()))
            .unwrap()
            .unwrap();
        assert_eq!(token.expose(), TOKEN);
        // Neither source: no token. An empty variable is no variable.
        assert!(resolve(None, None).unwrap().is_none());
        assert!(resolve(None, Some(OsString::new())).unwrap().is_none());
    }

    #[test]
    fn leading_flag_is_split_off_and_only_a_leading_one() {
        let (rest, path) = split_flag(&args(&[
            "herdr",
            "--caller-token-file",
            "/t",
            "agent",
            "list",
        ]))
        .unwrap();
        assert_eq!(rest, args(&["herdr", "agent", "list"]));
        assert_eq!(path, Some(PathBuf::from("/t")));
        let (rest, path) =
            split_flag(&args(&["herdr", "--caller-token-file=/t", "agent", "list"])).unwrap();
        assert_eq!(rest, args(&["herdr", "agent", "list"]));
        assert_eq!(path, Some(PathBuf::from("/t")));
        // Other leading options and their values stay.
        let (rest, path) = split_flag(&args(&[
            "herdr",
            "--session",
            "work",
            "--caller-token-file",
            "/t",
            "agent",
            "list",
        ]))
        .unwrap();
        assert_eq!(rest, args(&["herdr", "--session", "work", "agent", "list"]));
        assert_eq!(path, Some(PathBuf::from("/t")));
        // After the command it is the command's own argument, not ours.
        let input = args(&["herdr", "agent", "list", "--caller-token-file", "/t"]);
        assert_eq!(split_flag(&input).unwrap(), (input, None));
        let input = args(&[
            "herdr",
            "agent",
            "start",
            "x",
            "--",
            "--caller-token-file",
            "/t",
        ]);
        assert_eq!(split_flag(&input).unwrap(), (input, None));
        // Missing value, empty value and a repeat are refused.
        assert!(split_flag(&args(&["herdr", "--caller-token-file"])).is_err());
        assert!(split_flag(&args(&["herdr", "--caller-token-file=", "agent"])).is_err());
        assert!(split_flag(&args(&[
            "herdr",
            "--caller-token-file",
            "/a",
            "--caller-token-file",
            "/b",
            "agent"
        ]))
        .is_err());
    }
}
