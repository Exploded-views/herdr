// herdr-fork: `herdr access log` prints the input-origin log, newest first, from the server or (`--file`) from the rotated files; it never shows what was typed. Unofficial stopgap; see FORK.md.
use std::io::{self, Write};
use std::path::Path;

use crate::access::input_log::LOG_FILE;
use crate::api::schema::{AccessInputEvent, AccessLogParams, Method, ResponseResult};

use super::{call, fail, report, request, usage, Send};

struct LogArgs {
    limit: Option<u32>,
    json: bool,
    file: bool,
}

fn parse_log(args: &[String]) -> Result<LogArgs, String> {
    let mut parsed = LogArgs {
        limit: None,
        json: false,
        file: false,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => parsed.json = true,
            "--file" => parsed.file = true,
            "--limit" => {
                let value = iter.next().ok_or("--limit needs a value")?;
                parsed.limit = Some(parse_limit(value)?);
            }
            flag if flag.starts_with("--limit=") => {
                parsed.limit = Some(parse_limit(&flag["--limit=".len()..])?);
            }
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            extra => return Err(format!("unexpected argument {extra}")),
        }
    }
    Ok(parsed)
}

fn parse_limit(value: &str) -> Result<u32, String> {
    match value.parse::<u32>() {
        Ok(limit) if limit > 0 => Ok(limit),
        _ => Err("--limit must be a positive number".into()),
    }
}

pub(super) fn run_log(
    args: &[String],
    send: &mut Send<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<i32> {
    let parsed = match parse_log(args) {
        Ok(parsed) => parsed,
        Err(message) => return usage(err, &message),
    };
    let limit = parsed.limit.map(|limit| limit as usize);
    let (node, mut events, dropped) = if parsed.file {
        // The server may be down: read what its writer left in the data directory.
        read_files(&crate::session::data_dir())
    } else {
        let method = Method::AccessLog(AccessLogParams {
            limit: parsed.limit,
        });
        match call(send, &request(method)) {
            Ok(ResponseResult::AccessInputLog {
                node,
                events,
                dropped,
            }) => (node, events, dropped),
            Ok(_) => return fail(err, "unexpected response from the server"),
            Err(failure) => return report(err, &failure),
        }
    };
    if parsed.file {
        // Files are oldest first; the server's answer is already newest first.
        events.reverse();
    }
    if let Some(limit) = limit {
        events.truncate(limit);
    }
    if parsed.json {
        let document = serde_json::json!({ "node": node, "events": events, "dropped": dropped });
        writeln!(out, "{document}")?;
        return Ok(0);
    }
    for event in &events {
        writeln!(out, "{}", format_event(event))?;
    }
    if dropped > 0 {
        writeln!(out, "dropped: {dropped}")?;
    }
    Ok(0)
}

/// `time  pane  path/route  class[:principal]  client  host  bytes  decision  ×count`
pub(super) fn format_event(event: &AccessInputEvent) -> String {
    let class = match &event.principal {
        Some(principal) => format!("{}:{}", printable(&event.class), printable(principal)),
        None => printable(&event.class),
    };
    format!(
        "{}\t{}\t{}/{}\t{}\t{}\t{}\t{}\t{}\t×{}",
        format_time(event.at_ms),
        printable(&event.pane),
        printable(&event.path),
        printable(&event.route),
        class,
        printable(&event.client),
        printable(&event.host),
        event.bytes,
        printable(&event.decision),
        event.count
    )
}

/// Characters that print as nothing or change how the text around them is laid out: Unicode
/// format characters (Cf: soft hyphen, zero-width and bidi marks and overrides, isolates,
/// the word joiner and invisible operators, the byte order mark, interlinear annotation) and
/// the line and paragraph separators (Zl, Zp).
fn is_layout_character(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
    )
}

/// A field with every control and layout character written out (`\u{1b}`, `\u{202e}`), so a
/// line read from a file that another process can edit can't move the cursor, set the title,
/// start a new line or reorder the columns it is shown in.
fn printable(field: &str) -> String {
    field
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else if is_layout_character(c) {
                c.escape_unicode().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// UTC, to the second.
fn format_time(at_ms: u64) -> String {
    let nanos = i128::from(at_ms) * 1_000_000;
    match time::OffsetDateTime::from_unix_timestamp_nanos(nanos) {
        Ok(at) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            at.year(),
            u8::from(at.month()),
            at.day(),
            at.hour(),
            at.minute(),
            at.second()
        ),
        Err(_) => at_ms.to_string(),
    }
}

/// Entries from `dir`'s log files, oldest first, the node named in the newest header, and
/// the sum of the drop markers.
fn read_files(dir: &Path) -> (String, Vec<AccessInputEvent>, u64) {
    let mut node = String::from("node");
    let mut events = Vec::new();
    let mut dropped = 0u64;
    for name in [format!("{LOG_FILE}.1"), LOG_FILE.to_string()] {
        let Ok(text) = std::fs::read_to_string(dir.join(name)) else {
            continue;
        };
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if let Some(name) = value.get("node").and_then(|node| node.as_str()) {
                node = name.to_owned();
            } else if let Some(n) = value.get("dropped").and_then(|n| n.as_u64()) {
                dropped += n;
            } else if let Ok(event) = serde_json::from_value::<AccessInputEvent>(value) {
                events.push(event);
            }
        }
    }
    (node, events, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::registry::TestDir;

    fn event(at_ms: u64, route: &str, class: &str, principal: Option<&str>) -> AccessInputEvent {
        AccessInputEvent {
            at_ms,
            pane: "w1:p1".into(),
            terminal: "term_1".into(),
            path: "api".into(),
            route: route.into(),
            class: class.into(),
            principal: principal.map(str::to_owned),
            client: "api:3".into(),
            host: "local".into(),
            bytes: 42,
            decision: "allow".into(),
            count: 3,
        }
    }

    fn answer(events: Vec<AccessInputEvent>, dropped: u64) -> serde_json::Value {
        serde_json::to_value(crate::api::schema::SuccessResponse {
            id: "cli:request".into(),
            result: ResponseResult::AccessInputLog {
                node: "node-a".into(),
                events,
                dropped,
            },
        })
        .unwrap()
    }

    fn run(argv: &[&str], response: serde_json::Value) -> (i32, String, String, Vec<Method>) {
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut seen = Vec::new();
        let code = run_log(
            &args,
            &mut |request| {
                seen.push(request.method.clone());
                Ok(response.clone())
            },
            &mut out,
            &mut err,
        )
        .unwrap();
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
            seen,
        )
    }

    #[test]
    fn access_log_prints_newest_first_without_content() {
        let newest = event(1_700_000_100_000, "pane.send_text", "caller", Some("ci"));
        let older = event(1_700_000_000_000, "agent.prompt", "outside", None);
        let (code, out, err, seen) = run(
            &["--limit", "5"],
            answer(vec![newest.clone(), older.clone()], 0),
        );
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            seen,
            vec![Method::AccessLog(AccessLogParams { limit: Some(5) })]
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert_eq!(
            lines[0],
            "2023-11-14 22:15:00\tw1:p1\tapi/pane.send_text\tcaller:ci\tapi:3\tlocal\t42\tallow\t×3"
        );
        assert!(lines[1].contains("api/agent.prompt\toutside\t"));
        assert!(lines[1].starts_with("2023-11-14 22:13:20"));
        // Nine columns, none of which could hold what was typed.
        for line in &lines {
            assert_eq!(line.split('\t').count(), 9, "{line}");
        }
        assert!(!out.contains("dropped"));
    }

    #[test]
    fn access_log_shows_dropped_and_prints_json() {
        let (code, out, _, _) = run(
            &[],
            answer(vec![event(1, "pane.send_text", "outside", None)], 7),
        );
        assert_eq!(code, 0);
        assert!(out.lines().any(|line| line == "dropped: 7"), "{out}");
        let (_, out, _, _) = run(
            &["--json"],
            answer(vec![event(1, "pane.send_text", "outside", None)], 7),
        );
        let value: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(value["node"], "node-a");
        assert_eq!(value["dropped"], 7);
        assert_eq!(value["events"][0]["route"], "pane.send_text");
    }

    #[test]
    fn access_log_refusal_is_printed_with_its_code() {
        let refused =
            serde_json::json!({"id": "x", "error": {"code": "access_denied", "message": "no"}});
        let (code, out, err, _) = run(&[], refused);
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert!(err.contains("access_denied"));
    }

    #[test]
    fn access_log_usage_errors_never_connect() {
        for argv in [
            &["--limit"][..],
            &["--limit", "0"],
            &["--bogus"],
            &["extra"],
        ] {
            let (code, _, err, seen) = run(argv, answer(Vec::new(), 0));
            assert_eq!(code, 2, "{argv:?}");
            assert!(seen.is_empty());
            assert!(err.contains("Usage"));
        }
    }

    #[test]
    fn access_log_file_reads_both_files_newest_first() {
        let dir = TestDir::new("cli-log-file");
        let old = event(1_000, "agent.prompt", "outside", None);
        let new = event(2_000, "pane.send_text", "outside", None);
        std::fs::write(
            dir.path().join(format!("{LOG_FILE}.1")),
            format!(
                "{{\"node\":\"old\",\"started_ms\":1}}\n{}\n",
                serde_json::to_string(&old).unwrap()
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path().join(LOG_FILE),
            format!(
                "{{\"node\":\"node-a\",\"started_ms\":2}}\n{{\"dropped\":4,\"at_ms\":5}}\n{}\nnot json\n",
                serde_json::to_string(&new).unwrap()
            ),
        )
        .unwrap();
        let (node, events, dropped) = read_files(dir.path());
        assert_eq!(node, "node-a");
        assert_eq!(dropped, 4);
        assert_eq!(events, vec![old, new]);
    }

    #[test]
    fn access_log_escapes_control_characters_from_the_file() {
        let mut hostile = event(
            1_700_000_000_000,
            "pane.send_text",
            "caller",
            Some("\u{1b}]0;owned\u{7}"),
        );
        hostile.pane = "w1\u{1b}[2J".into();
        hostile.client = "api:1\r\nforged line".into();
        let line = format_event(&hostile);
        assert!(
            !line.contains('\u{1b}') && !line.contains('\u{7}'),
            "{line:?}"
        );
        assert!(!line.contains('\r') && !line.contains('\n'), "{line:?}");
        assert!(line.contains("\\u{1b}"), "{line}");
        assert_eq!(line.split('\t').count(), 9);
    }

    #[test]
    fn access_log_escapes_format_characters_in_every_field_and_branch() {
        let bidi = "\u{202e}evil\u{2066}\u{200b}\u{feff}\u{2028}x";
        for principal in [Some(bidi), None] {
            let mut hostile = event(1_700_000_000_000, "pane.send_text", bidi, principal);
            hostile.client = format!("api:1{bidi}");
            hostile.pane = format!("w1{bidi}");
            hostile.host = format!("local{bidi}");
            hostile.decision = format!("allow{bidi}");
            let line = format_event(&hostile);
            for c in line.chars() {
                assert!(
                    !c.is_control() || c == '\t',
                    "control character {:?} in {line:?}",
                    c
                );
                assert!(
                    !matches!(c, '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}'),
                    "format character {:?} in {line:?}",
                    c
                );
            }
            assert_eq!(line.split('\t').count(), 9, "{line:?}");
            assert!(line.contains("\\u{202e}"), "{line}");
        }
    }
}
