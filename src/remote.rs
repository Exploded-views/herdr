// herdr-fork: the remote api bridge stamps caller_origin "remote" on the first request line and fails closed if that line is bad. Unofficial stopgap; see FORK.md.
mod args;
mod attach;
mod host;
mod process;
mod restart_policy;
mod saved;
#[cfg(unix)]
mod ssh_agent;

pub(crate) use args::*;
pub(crate) use attach::*;
pub(crate) use host::run_remote_client_bridge;
pub(crate) use saved::*;

/// Room left under the server's first-line cap for the stamp the bridge adds.
const REMOTE_STAMP_HEADROOM: usize = 64;

/// The one line the bridge prints when it refuses a first line it can't stamp.
const REMOTE_BRIDGE_REFUSAL: &str = r#"{"id":"","error":{"code":"invalid_request","message":"remote bridge: first line is not a JSON request object"}}"#;

/// Set `"caller_origin":"remote"` on a JSON-object request line and return it
/// with its trailing newline. The caller has already checked the line is an
/// object (`read_remote_first_line`); anything else comes back unchanged.
fn stamp_remote_origin(line: &[u8]) -> Vec<u8> {
    let Ok(serde_json::Value::Object(mut object)) = serde_json::from_slice(line) else {
        return line.to_vec();
    };
    object.insert(
        "caller_origin".into(),
        serde_json::Value::String("remote".into()),
    );
    let mut stamped = serde_json::to_vec(&object).unwrap_or_else(|_| line.to_vec());
    stamped.push(b'\n');
    stamped
}

/// Read the first line one byte at a time, so nothing past it is consumed, and
/// return it stamped. `None` means the line can't be trusted: no `\n` before
/// EOF or the cap, a read error, or not a JSON object.
fn read_remote_first_line(input: &mut impl std::io::Read) -> Option<Vec<u8>> {
    let cap = crate::api::MAX_INITIAL_REQUEST_BYTES.saturating_sub(REMOTE_STAMP_HEADROOM);
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match input.read(&mut byte) {
            Ok(0) => return None,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                if line.len() >= cap {
                    return None;
                }
                line.push(byte[0]);
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    match serde_json::from_slice::<serde_json::Value>(&line) {
        Ok(serde_json::Value::Object(_)) => Some(stamp_remote_origin(&line)),
        _ => None,
    }
}

pub(crate) fn run_remote_api_bridge(args: &[String]) -> std::io::Result<()> {
    match args {
        [] => {
            use std::io::Write as _;

            // Stamp before connecting: a first line we can't vouch for never reaches the API.
            let Some(stamped) = read_remote_first_line(&mut std::io::stdin().lock()) else {
                let mut stdout = std::io::stdout().lock();
                writeln!(stdout, "{REMOTE_BRIDGE_REFUSAL}")?;
                stdout.flush()?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "remote bridge: first line is not a JSON request object",
                ));
            };
            let path = crate::api::socket_path();
            let mut stream = crate::ipc::connect_local_stream(&path).map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "failed to connect to remote Herdr API socket {}: {error}",
                        path.display()
                    ),
                )
            })?;
            stream.write_all(&stamped)?;
            stream.flush()?;
            crate::platform::forward_remote_bridge_stdio(stream, false)
        }
        [flag] if flag == "--check" => {
            println!("herdr-api-bridge-v1");
            Ok(())
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "usage: herdr remote-api-bridge [--check]",
        )),
    }
}

pub(crate) fn print_saved_ssh_error_hint(err: &std::io::Error, target: &str) {
    if is_remote_host_key_error(err) {
        eprintln!(
            "hint: saved machines use strict host-key checking; add the host key to the configured known_hosts file, then retry."
        );
    } else {
        print_remote_error_hint(err, target);
    }
}

pub(crate) fn print_remote_error_hint(err: &std::io::Error, target: &str) {
    if is_remote_auth_error(err) {
        eprintln!(
            "hint: verify SSH access first with `{}`.",
            ssh_check_command(target)
        );
        eprintln!(
            "hint: if your SSH key has a passphrase, load it into ssh-agent with `ssh-add` before running `herdr --remote`."
        );
    }
}

fn is_remote_host_key_error(err: &std::io::Error) -> bool {
    let message = err.to_string().to_ascii_lowercase();
    message.contains("host key verification failed")
        || message.contains("remote host identification has changed")
}

fn is_remote_auth_error(err: &std::io::Error) -> bool {
    let message = err.to_string();
    message.contains("Permission denied")
        && (message.contains("(publickey")
            || message.contains("(keyboard-interactive")
            || message.contains("(password"))
}

fn ssh_check_command(target: &str) -> String {
    format!("ssh {}", shell_quote(target))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_host_key_error_matches_ssh_diagnostics() {
        for message in [
            "Host key verification failed.",
            "REMOTE HOST IDENTIFICATION HAS CHANGED!",
        ] {
            assert!(is_remote_host_key_error(&std::io::Error::other(message)));
        }
        assert!(!is_remote_host_key_error(&std::io::Error::other(
            "server closed connection"
        )));
    }

    #[test]
    fn remote_auth_error_matches_ssh_auth_denied() {
        let err = std::io::Error::other(
            "remote platform detection failed: user@host: Permission denied (publickey).",
        );

        assert!(is_remote_auth_error(&err));
    }

    #[test]
    fn remote_auth_error_matches_keyboard_interactive_denied() {
        let err = std::io::Error::other(
            "remote server status failed: user@host: Permission denied (keyboard-interactive).",
        );

        assert!(is_remote_auth_error(&err));
    }

    #[test]
    fn remote_auth_error_ignores_non_auth_errors() {
        let err = std::io::Error::other("remote platform detection failed: unsupported platform");

        assert!(!is_remote_auth_error(&err));
    }

    #[test]
    fn ssh_check_command_quotes_remote_target() {
        assert_eq!(ssh_check_command("host name"), "ssh 'host name'");
    }

    #[test]
    fn remote_bridge_stamps_origin_on_first_line() {
        let stamped = stamp_remote_origin(
            br#"{"id":"1","method":"ping","params":{},"caller_origin":"local","caller_token":"t"}"#,
        );
        assert_eq!(stamped.last(), Some(&b'\n'));
        let value: serde_json::Value = serde_json::from_slice(&stamped).unwrap();
        assert_eq!(value["caller_origin"], "remote");
        assert_eq!(value["caller_token"], "t");
        assert_eq!(value["method"], "ping");

        let first = br#"{"id":"1","method":"ping","params":{}}"#;
        let mut bytes = first.to_vec();
        bytes.extend_from_slice(b"\nrest of stream");
        let mut input = std::io::Cursor::new(bytes);
        let line = read_remote_first_line(&mut input).expect("object line is stamped");
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(value["caller_origin"], "remote");
        // Nothing past the first newline was consumed.
        assert_eq!(input.position(), first.len() as u64 + 1);
    }

    #[test]
    fn remote_bridge_refuses_unparseable_first_line() {
        for bad in [
            &b"not json\n"[..],
            b"[1,2]\n",
            b"\"string\"\n",
            b"\n",
            b"{\"id\":\"1\"",
        ] {
            let mut input = std::io::Cursor::new(bad.to_vec());
            assert!(read_remote_first_line(&mut input).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn remote_bridge_refuses_oversized_first_line() {
        let cap = crate::api::MAX_INITIAL_REQUEST_BYTES - REMOTE_STAMP_HEADROOM;
        let pad = |len: usize| {
            let mut line = br#"{"id":"1","pad":""#.to_vec();
            line.resize(len - 2, b'a');
            line.extend_from_slice(b"\"}\n");
            line
        };
        let mut over = std::io::Cursor::new(pad(cap + 1));
        assert!(read_remote_first_line(&mut over).is_none());
        let mut fits = std::io::Cursor::new(pad(cap));
        assert!(read_remote_first_line(&mut fits).is_some());
    }
}
