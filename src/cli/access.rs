// herdr-fork: `herdr access caller` (add, list, rotate, retarget, remove) and `herdr access pane` (set, get); tokens go to a 0600 file and are never printed. Unofficial stopgap; see FORK.md.
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::api::schema::{Method, Request, ResponseResult};

const HELP: &str = "Usage:
  herdr access caller add <name> --target-pane <pane> --token-file <path>
  herdr access caller list
  herdr access caller rotate <name> --token-file <path>
  herdr access caller retarget <name> --target-pane <pane>
  herdr access caller remove <name>
  herdr access pane set <pane> --accept-from <list>
  herdr access pane get <pane>

A caller holds a secret token and may send input only to the pane it is bound to.
The token is written to the token file (mode 0600) and is never printed.
`add` refuses a token file that already exists. `rotate` replaces the file, and
the old token stops working.
<list> is `any`, or a comma-separated or repeated list of `none`, `outside` and
`caller:<name>`. `pane set` is for a caller outside every pane only; `any` removes
the policy and opens the pane on purpose.";

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
        Some("pane") => run_pane(&args[1..], send, out, err),
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
    accept_from: Vec<String>,
}

/// `--accept-from` takes a comma-separated list, and may repeat.
fn split_list(value: &str) -> Result<Vec<String>, String> {
    let entries: Vec<String> = value.split(',').map(str::to_string).collect();
    if entries.iter().any(String::is_empty) {
        return Err("--accept-from has an empty entry".into());
    }
    Ok(entries)
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
            "--accept-from" => {
                let value = iter.next().ok_or("--accept-from needs a value")?;
                parsed.accept_from.extend(split_list(value)?);
            }
            flag if flag.starts_with("--accept-from=") => {
                parsed
                    .accept_from
                    .extend(split_list(&flag["--accept-from=".len()..])?);
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
            let outcome = call(
                send,
                &request(Method::AccessCallerAdd(
                    crate::api::schema::AccessCallerAddParams { name, target_pane },
                )),
            );
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
            let outcome = call(
                send,
                &request(Method::AccessCallerRotate(
                    crate::api::schema::AccessCallerNameParams { name },
                )),
            );
            deliver_token(outcome, file, &tmp, Some(&token_file), out, err)
        }
        "list" => {
            if parsed.name.is_some() || parsed.target_pane.is_some() || parsed.token_file.is_some()
            {
                return usage(err, "list takes no arguments");
            }
            let outcome = call(send, &request(Method::AccessCallerList(Default::default())));
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
                Err(failure) => report(err, &failure),
            }
        }
        "retarget" => {
            let (Some(name), Some(target_pane)) = (parsed.name, parsed.target_pane) else {
                return usage(err, "retarget needs <name> and --target-pane");
            };
            let outcome = call(
                send,
                &request(Method::AccessCallerRetarget(
                    crate::api::schema::AccessCallerRetargetParams { name, target_pane },
                )),
            );
            acknowledge(outcome, err)
        }
        "remove" => {
            let Some(name) = parsed.name else {
                return usage(err, "remove needs <name>");
            };
            let outcome = call(
                send,
                &request(Method::AccessCallerRemove(
                    crate::api::schema::AccessCallerNameParams { name },
                )),
            );
            acknowledge(outcome, err)
        }
        _ => usage(err, &format!("unknown access caller command {action}")),
    }
}

fn run_pane(
    args: &[String],
    send: &mut Send<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<i32> {
    let Some(action) = args.first().map(String::as_str) else {
        return usage(err, "missing access pane command");
    };
    let parsed = match parse(&args[1..]) {
        Ok(parsed) => parsed,
        Err(message) => return usage(err, &message),
    };
    if parsed.target_pane.is_some() || parsed.token_file.is_some() {
        return usage(err, "access pane takes only <pane> and --accept-from");
    }
    let method = match action {
        "set" => match (parsed.name, parsed.accept_from.is_empty()) {
            (Some(pane_id), false) => {
                Method::AccessPaneSet(crate::api::schema::AccessPaneSetParams {
                    pane_id,
                    accept_from: parsed.accept_from,
                })
            }
            _ => return usage(err, "set needs <pane> and --accept-from"),
        },
        "get" => match (parsed.name, parsed.accept_from.is_empty()) {
            (Some(pane_id), true) => {
                Method::AccessPaneGet(crate::api::schema::AccessPaneGetParams { pane_id })
            }
            _ => return usage(err, "get needs <pane> and takes no --accept-from"),
        },
        _ => return usage(err, &format!("unknown access pane command {action}")),
    };
    let outcome = call(send, &request(method));
    match outcome {
        Ok(ResponseResult::AccessPolicy {
            pane_id,
            binding,
            accept_from,
        }) => {
            writeln!(
                out,
                "{pane_id}\t{}\t{}",
                binding.as_deref().unwrap_or("-"),
                accept_from.join(",")
            )?;
            Ok(0)
        }
        Ok(_) => fail(err, "unexpected response from the server"),
        Err(failure) => report(err, &failure),
    }
}

/// Why a request failed, and the exit status for it. A caller token file that
/// cannot be used is a usage error (exit 2), like everywhere else in the CLI.
struct Failure {
    message: String,
    code: i32,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self { message, code: 1 }
    }
}

fn transport_failure(error: io::Error) -> Failure {
    match super::caller_token::usage_report(&error) {
        Some(message) => Failure { message, code: 2 },
        None => error.to_string().into(),
    }
}

fn call(send: &mut Send<'_>, request: &Request) -> Result<ResponseResult, Failure> {
    let response = send(request).map_err(transport_failure)?;
    Ok(result_of(&response)?)
}

fn report(err: &mut dyn Write, failure: &Failure) -> io::Result<i32> {
    writeln!(err, "{}", failure.message)?;
    Ok(failure.code)
}

fn fail(err: &mut dyn Write, message: &str) -> io::Result<i32> {
    writeln!(err, "{message}")?;
    Ok(1)
}

fn acknowledge(outcome: Result<ResponseResult, Failure>, err: &mut dyn Write) -> io::Result<i32> {
    match outcome {
        Ok(_) => Ok(0),
        Err(failure) => report(err, &failure),
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
    outcome: Result<ResponseResult, Failure>,
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
        Err(failure) => {
            drop(file);
            let _ = std::fs::remove_file(written);
            return report(err, &failure);
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
    fn unreadable_caller_token_file_exits_2_with_the_shared_message() {
        let unreadable = || {
            super::super::caller_token::unreadable_error(
                "caller token file /x/tok unreadable: nope".into(),
            )
        };
        let dir = TestDir::new("cli-unreadable-caller");
        let file = dir.path().join("out-token");
        let out_file = file.to_string_lossy().into_owned();
        let cases: Vec<Vec<&str>> = vec![
            vec!["caller", "list"],
            vec!["caller", "remove", "ops"],
            vec!["pane", "get", "p_1"],
            vec![
                "caller",
                "add",
                "ops",
                "--target-pane",
                "p_1",
                "--token-file",
                &out_file,
            ],
        ];
        for argv in cases {
            let (code, out, err) = run(&argv, |_| Err(unreadable()));
            assert_eq!(code, 2, "{argv:?}");
            assert!(out.is_empty(), "{argv:?}");
            assert_eq!(
                err.trim_end(),
                "error: caller token file /x/tok unreadable: nope",
                "{argv:?}"
            );
        }
        // The half-made token file is not left behind.
        assert!(!file.exists());
        // Other transport errors still exit 1.
        let (code, _, _) = run(&["caller", "list"], |_| Err(io::Error::other("boom")));
        assert_eq!(code, 1);
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

    fn policy_response(binding: Option<&str>, accept_from: &[&str]) -> serde_json::Value {
        let mut result = serde_json::json!({
            "type": "access_policy",
            "pane_id": "w1:p1",
            "accept_from": accept_from,
        });
        if let Some(binding) = binding {
            result["binding"] = binding.into();
        }
        serde_json::json!({"id": "cli:request", "result": result})
    }

    fn sent_pane_set(argv: &[&str]) -> (i32, String, String, Vec<Method>) {
        let mut seen = Vec::new();
        let (code, out, err) = run(argv, |request| {
            seen.push(request.method.clone());
            Ok(policy_response(
                Some("acc_00000000000000000000000000000001"),
                &["outside", "caller:ops"],
            ))
        });
        (code, out, err, seen)
    }

    #[test]
    fn pane_set_sends_the_parsed_list_and_prints_the_policy() {
        let (code, out, err, seen) = sent_pane_set(&[
            "pane",
            "set",
            "w1:p1",
            "--accept-from",
            "outside,caller:ops",
        ]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            out,
            "w1:p1\tacc_00000000000000000000000000000001\toutside,caller:ops\n"
        );
        assert_eq!(
            seen,
            vec![Method::AccessPaneSet(
                crate::api::schema::AccessPaneSetParams {
                    pane_id: "w1:p1".into(),
                    accept_from: vec!["outside".into(), "caller:ops".into()],
                }
            )]
        );
        // Repeats accumulate, `=` works, and `any` goes through as the sole entry.
        for (argv, expected) in [
            (
                &[
                    "pane",
                    "set",
                    "w1:p1",
                    "--accept-from",
                    "outside",
                    "--accept-from=caller:ops",
                ][..],
                vec!["outside", "caller:ops"],
            ),
            (
                &["pane", "set", "--accept-from=any", "w1:p1"][..],
                vec!["any"],
            ),
            (
                &["pane", "set", "w1:p1", "--accept-from", "none"][..],
                vec!["none"],
            ),
        ] {
            let (code, _, err, seen) = sent_pane_set(argv);
            assert_eq!(code, 0, "{err}");
            let Method::AccessPaneSet(params) = &seen[0] else {
                panic!("{seen:?}")
            };
            assert_eq!(params.accept_from, expected, "{argv:?}");
        }
    }

    #[test]
    fn pane_get_prints_the_policy_and_a_dash_for_no_binding() {
        let mut seen = Vec::new();
        let (code, out, _) = run(&["pane", "get", "w1:p1"], |request| {
            seen.push(request.method.clone());
            Ok(policy_response(None, &["any"]))
        });
        assert_eq!(code, 0);
        assert_eq!(out, "w1:p1\t-\tany\n");
        assert_eq!(
            seen,
            vec![Method::AccessPaneGet(
                crate::api::schema::AccessPaneGetParams {
                    pane_id: "w1:p1".into()
                }
            )]
        );
    }

    #[test]
    fn pane_errors_are_printed_with_their_code() {
        let (code, out, err) = run(&["pane", "set", "w9:p9", "--accept-from", "none"], |_| {
            Ok(serde_json::json!({"id": "x", "error": {
                "code": "pane_not_found", "message": "pane w9:p9 not found"}}))
        });
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert!(
            err.contains("pane_not_found: pane w9:p9 not found"),
            "{err}"
        );
    }

    #[test]
    fn pane_usage_errors_never_connect() {
        for argv in [
            &["pane"][..],
            &["pane", "bogus"],
            &["pane", "set"],
            &["pane", "set", "w1:p1"],
            &["pane", "set", "--accept-from", "none"],
            &["pane", "set", "w1:p1", "--accept-from"],
            &["pane", "set", "w1:p1", "--accept-from", ""],
            &["pane", "set", "w1:p1", "--accept-from", "none,,outside"],
            &["pane", "set", "w1:p1", "w1:p2", "--accept-from", "none"],
            &[
                "pane",
                "set",
                "w1:p1",
                "--accept-from",
                "none",
                "--token-file",
                "t",
            ],
            &[
                "pane",
                "set",
                "w1:p1",
                "--accept-from",
                "none",
                "--target-pane",
                "p",
            ],
            &["pane", "get"],
            &["pane", "get", "w1:p1", "w1:p2"],
            &["pane", "get", "w1:p1", "--accept-from", "none"],
        ] {
            let (code, _, _) = run(argv, |_| panic!("usage errors must not connect: {argv:?}"));
            assert_eq!(code, 2, "{argv:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn rotated_token_file_is_mode_0600_and_never_wider_than_the_old_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new("cli-rotate-mode");
        let file = dir.path().join("token");
        std::fs::write(&file, "old\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let path = file.to_str().unwrap();
        let (code, _, err) = run(&["caller", "rotate", "ops", "--token-file", path], |_| {
            Ok(token_response())
        });
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
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
