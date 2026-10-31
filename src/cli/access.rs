// herdr-fork: `herdr access caller` (add, list, rotate, retarget, remove); tokens go to a 0600 file and are never printed. Unofficial stopgap; see FORK.md.
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::api::schema::{Method, Request, ResponseResult};

const HELP: &str = "Usage:
  herdr access caller add <name> --target-pane <pane> --token-file <path>
  herdr access caller list
  herdr access caller rotate <name> --token-file <path>
  herdr access caller retarget <name> --target-pane <pane>
  herdr access caller remove <name>

A caller holds a secret token and may send input only to the pane it is bound to.
The token is written to the token file (mode 0600) and is never printed.
`add` refuses a token file that already exists. `rotate` replaces the file, and
the old token stops working.";

pub(super) fn run_access_command(args: &[String]) -> io::Result<i32> {
    run_access(
        args,
        &mut |request| super::send_request(request),
        &mut io::stdout(),
        &mut io::stderr(),
    )
}

type Send<'a> = dyn FnMut(&Request) -> io::Result<serde_json::Value> + 'a;

fn run_access(
    args: &[String],
    send: &mut Send<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("caller") => run_caller(&args[1..], send, out, err),
        _ => {
            writeln!(err, "{HELP}")?;
            Ok(2)
        }
    }
}

fn usage(err: &mut dyn Write, message: &str) -> io::Result<i32> {
    writeln!(err, "{message}\n\n{HELP}")?;
    Ok(2)
}

#[derive(Default)]
struct Parsed {
    name: Option<String>,
    target_pane: Option<String>,
    token_file: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut parsed = Parsed::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--target-pane" => {
                let value = iter.next().ok_or("--target-pane needs a value")?;
                parsed.target_pane = Some(value.clone());
            }
            "--token-file" => {
                let value = iter.next().ok_or("--token-file needs a value")?;
                parsed.token_file = Some(PathBuf::from(value));
            }
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            name if parsed.name.is_none() => parsed.name = Some(name.to_string()),
            extra => return Err(format!("unexpected argument {extra}")),
        }
    }
    Ok(parsed)
}

fn request(method: Method) -> Request {
    Request {
        id: "cli:request".into(),
        method,
    }
}

/// `Ok(result)` for a success response, or the printed error text.
fn result_of(response: &serde_json::Value) -> Result<ResponseResult, String> {
    if let Some(error) = response.get("error") {
        let code = error
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        let message = error.get("message").and_then(|v| v.as_str()).unwrap_or("");
        return Err(format!("{code}: {message}"));
    }
    response
        .get("result")
        .cloned()
        .and_then(|result| serde_json::from_value(result).ok())
        .ok_or_else(|| "unexpected response from the server".to_string())
}

fn run_caller(
    args: &[String],
    send: &mut Send<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<i32> {
    let Some(action) = args.first().map(String::as_str) else {
        return usage(err, "missing access caller command");
    };
    let parsed = match parse(&args[1..]) {
        Ok(parsed) => parsed,
        Err(message) => return usage(err, &message),
    };
    match action {
        "add" => {
            let (Some(name), Some(target_pane), Some(token_file)) =
                (parsed.name, parsed.target_pane, parsed.token_file)
            else {
                return usage(err, "add needs <name>, --target-pane and --token-file");
            };
            // Claim the file first: an existing one is refused before the
            // server is asked for a token that would then have nowhere to go.
            let file = match crate::platform::create_private_file(&token_file) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    writeln!(
                        err,
                        "token file {} already exists; refusing to overwrite it",
                        token_file.display()
                    )?;
                    return Ok(1);
                }
                Err(e) => return Err(e),
            };
            let outcome = send(&request(Method::AccessCallerAdd(
                crate::api::schema::AccessCallerAddParams { name, target_pane },
            )))
            .map_err(|e| e.to_string())
            .and_then(|response| result_of(&response));
            deliver_token(outcome, file, &token_file, None, out, err)
        }
        "rotate" => {
            let (Some(name), Some(token_file)) = (parsed.name, parsed.token_file) else {
                return usage(err, "rotate needs <name> and --token-file");
            };
            let tmp = sibling_tmp(&token_file);
            // A leftover from a crashed run is ours to clear.
            let _ = std::fs::remove_file(&tmp);
            let file = crate::platform::create_private_file(&tmp)?;
            let outcome = send(&request(Method::AccessCallerRotate(
                crate::api::schema::AccessCallerNameParams { name },
            )))
            .map_err(|e| e.to_string())
            .and_then(|response| result_of(&response));
            deliver_token(outcome, file, &tmp, Some(&token_file), out, err)
        }
        "list" => {
            if parsed.name.is_some() || parsed.target_pane.is_some() || parsed.token_file.is_some()
            {
                return usage(err, "list takes no arguments");
            }
            let outcome = send(&request(Method::AccessCallerList(Default::default())))
                .map_err(|e| e.to_string())
                .and_then(|response| result_of(&response));
            match outcome {
                Ok(ResponseResult::AccessCallers { callers }) => {
                    for caller in callers {
                        writeln!(
                            out,
                            "{}\t{}\t{}",
                            caller.name,
                            caller.binding,
                            caller.pane_id.as_deref().unwrap_or("-")
                        )?;
                    }
                    Ok(0)
                }
                Ok(_) => fail(err, "unexpected response from the server"),
                Err(message) => fail(err, &message),
            }
        }
        "retarget" => {
            let (Some(name), Some(target_pane)) = (parsed.name, parsed.target_pane) else {
                return usage(err, "retarget needs <name> and --target-pane");
            };
            let outcome = send(&request(Method::AccessCallerRetarget(
                crate::api::schema::AccessCallerRetargetParams { name, target_pane },
            )))
            .map_err(|e| e.to_string())
            .and_then(|response| result_of(&response));
            acknowledge(outcome, err)
        }
        "remove" => {
            let Some(name) = parsed.name else {
                return usage(err, "remove needs <name>");
            };
            let outcome = send(&request(Method::AccessCallerRemove(
                crate::api::schema::AccessCallerNameParams { name },
            )))
            .map_err(|e| e.to_string())
            .and_then(|response| result_of(&response));
            acknowledge(outcome, err)
        }
        _ => usage(err, &format!("unknown access caller command {action}")),
    }
}

fn fail(err: &mut dyn Write, message: &str) -> io::Result<i32> {
    writeln!(err, "{message}")?;
    Ok(1)
}

fn acknowledge(outcome: Result<ResponseResult, String>, err: &mut dyn Write) -> io::Result<i32> {
    match outcome {
        Ok(_) => Ok(0),
        Err(message) => fail(err, &message),
    }
}

fn sibling_tmp(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp.{}", std::process::id()));
    path.with_file_name(name)
}

/// Put a minted token into `file` (already created at `written`, mode 0600)
/// and print the binding id. With `rename_to`, `written` is a temp file that
/// replaces that path once the token is in it. On any failure `written` is
/// removed. The token goes nowhere else.
fn deliver_token(
    outcome: Result<ResponseResult, String>,
    mut file: std::fs::File,
    written: &Path,
    rename_to: Option<&Path>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<i32> {
    let (binding, token) = match outcome {
        Ok(ResponseResult::AccessCallerToken { binding, token, .. }) => (binding, token),
        Ok(_) => {
            drop(file);
            let _ = std::fs::remove_file(written);
            return fail(err, "unexpected response from the server");
        }
        Err(message) => {
            drop(file);
            let _ = std::fs::remove_file(written);
            return fail(err, &message);
        }
    };
    let saved = writeln!(file, "{}", token.expose())
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            match rename_to {
                Some(target) => std::fs::rename(written, target),
                None => Ok(()),
            }
        });
    if let Err(e) = saved {
        let _ = std::fs::remove_file(written);
        // The server already holds the new token; say how to get another one.
        writeln!(
            err,
            "could not write the token file ({}); run the command again to mint a new token",
            e.kind()
        )?;
        return Ok(1);
    }
    writeln!(out, "{binding}")?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::registry::TestDir;
    use crate::api::schema::CallerToken;

    const TOKEN: &str = "ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12";

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn token_response() -> serde_json::Value {
        serde_json::to_value(crate::api::schema::SuccessResponse {
            id: "cli:request".into(),
            result: ResponseResult::AccessCallerToken {
                name: "ops".into(),
                binding: "acc_00000000000000000000000000000001".into(),
                token: CallerToken::new(TOKEN.into()),
            },
        })
        .unwrap()
    }

    fn run(
        argv: &[&str],
        mut send: impl FnMut(&Request) -> io::Result<serde_json::Value>,
    ) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_access(&args(argv), &mut send, &mut out, &mut err).unwrap();
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn caller_add_refuses_existing_token_file() {
        let dir = TestDir::new("cli-existing");
        let file = dir.path().join("token");
        std::fs::write(&file, "keep me").unwrap();
        let path = file.to_str().unwrap();
        let (code, out, err) = run(
            &[
                "caller",
                "add",
                "ops",
                "--target-pane",
                "w1:p1",
                "--token-file",
                path,
            ],
            |_| panic!("the server must not be asked when the token file exists"),
        );
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert!(err.contains("already exists"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep me");
    }

    #[test]
    fn caller_add_writes_the_token_file_and_prints_only_the_binding() {
        let dir = TestDir::new("cli-add");
        let file = dir.path().join("token");
        let path = file.to_str().unwrap();
        let mut seen = Vec::new();
        let (code, out, err) = run(
            &[
                "caller",
                "add",
                "ops",
                "--target-pane",
                "w1:p1",
                "--token-file",
                path,
            ],
            |request| {
                seen.push(request.method.clone());
                Ok(token_response())
            },
        );
        assert_eq!(code, 0);
        assert_eq!(out, "acc_00000000000000000000000000000001\n");
        assert!(!out.contains(TOKEN) && !err.contains(TOKEN));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("{TOKEN}\n")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(matches!(seen[0], Method::AccessCallerAdd(_)));
    }

    #[test]
    fn caller_add_failure_leaves_no_token_file() {
        let dir = TestDir::new("cli-fail");
        let file = dir.path().join("token");
        let path = file.to_str().unwrap();
        let (code, out, err) = run(
            &[
                "caller",
                "add",
                "ops",
                "--target-pane",
                "w1:p1",
                "--token-file",
                path,
            ],
            |_| {
                Ok(serde_json::json!({"id": "cli:request", "error": {
                    "code": "no_access_policy",
                    "message": "pane w1:p1 has no access policy; set one first"
                }}))
            },
        );
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert!(err.contains("no_access_policy"));
        assert!(!file.exists());
    }

    #[test]
    fn caller_rotate_replaces_the_file_through_a_private_temp() {
        let dir = TestDir::new("cli-rotate");
        let file = dir.path().join("token");
        std::fs::write(&file, "old\n").unwrap();
        let path = file.to_str().unwrap();
        let (code, out, err) = run(&["caller", "rotate", "ops", "--token-file", path], |_| {
            // While the server answers, the old file is untouched.
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "old\n");
            Ok(token_response())
        });
        assert_eq!(code, 0, "{err}");
        assert!(!out.contains(TOKEN) && !err.contains(TOKEN));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("{TOKEN}\n")
        );
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1);
    }

    #[test]
    fn caller_rotate_failure_keeps_the_old_file() {
        let dir = TestDir::new("cli-rotate-fail");
        let file = dir.path().join("token");
        std::fs::write(&file, "old\n").unwrap();
        let path = file.to_str().unwrap();
        let (code, _, err) = run(&["caller", "rotate", "ops", "--token-file", path], |_| {
            Ok(
                serde_json::json!({"id": "x", "error": {"code": "caller_not_found", "message": "no caller named ops"}}),
            )
        });
        assert_eq!(code, 1);
        assert!(err.contains("caller_not_found"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old\n");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn caller_list_prints_names_and_bindings() {
        let (code, out, _) = run(&["caller", "list"], |_| {
            Ok(
                serde_json::json!({"id": "x", "result": {"type": "access_callers", "callers": [
                    {"name": "ops", "binding": "acc_00000000000000000000000000000001", "pane_id": "w1:p1"},
                    {"name": "gone", "binding": "acc_00000000000000000000000000000002"}
                ]}}),
            )
        });
        assert_eq!(code, 0);
        assert_eq!(
            out,
            "ops\tacc_00000000000000000000000000000001\tw1:p1\ngone\tacc_00000000000000000000000000000002\t-\n"
        );
    }

    #[test]
    fn usage_errors_never_connect() {
        for argv in [
            &["caller", "add", "ops"][..],
            &["caller", "add", "--target-pane", "p"],
            &["caller", "rotate", "ops"],
            &["caller", "remove"],
            &["caller", "retarget", "ops"],
            &["caller", "list", "extra"],
            &["caller", "bogus"],
            &["caller"],
            &["bogus"],
        ] {
            let (code, _, _) = run(argv, |_| panic!("usage errors must not connect: {argv:?}"));
            assert_eq!(code, 2, "{argv:?}");
        }
    }
}
