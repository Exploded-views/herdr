// herdr-fork: fork-only file. The platform-neutral part of the test-side launchd guard. Unofficial
// stopgap; see FORK.md.
//! The decisions that keep tests off the real macOS launchd domain, as pure functions so they run
//! (and are mutation-tested) on every unix platform, not only on macOS.

use std::ffi::OsStr;

/// Any non-empty value turns the launchd start off. Read at run time only.
pub(crate) const NO_LAUNCHD_ENV: &str = "HERDR_NO_LAUNCHD";
/// Labels of the real server jobs; nothing under test may name one.
pub(crate) const SERVER_LABEL_PREFIX: &str = "dev.herdr.server.";

/// Whether a launchd job may be created, given the switch's value and whether
/// this is a test build.
pub(crate) fn launchd_allowed(switch: Option<&OsStr>, under_test: bool) -> bool {
    !under_test && switch.is_none_or(|value| value.is_empty())
}

/// Whether the switch value turns launchd off.
pub(crate) fn switch_set(switch: Option<&OsStr>) -> bool {
    !launchd_allowed(switch, false)
}

/// The tripwire. `guarded` is true under test or with the switch set; `permitted` is true when a
/// test-only fake or the CI opt-in stands in for the real launchctl. A guarded call that names a
/// server job is always refused; any other guarded call is refused unless permitted.
pub(crate) fn check_call(args: &[&str], guarded: bool, permitted: bool) -> Result<(), String> {
    if !guarded {
        return Ok(());
    }
    if args.iter().any(|arg| arg.contains(SERVER_LABEL_PREFIX)) {
        return Err(format!(
            "launchctl is forbidden from naming a server job here: {args:?}"
        ));
    }
    if !permitted {
        return Err(format!(
            "launchctl is forbidden under test or {NO_LAUNCHD_ENV}: {args:?}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_decision_allows_launchd_unless_switched_off() {
        let empty = OsStr::new("");
        assert!(launchd_allowed(None, false));
        assert!(launchd_allowed(Some(empty), false));
        assert!(!launchd_allowed(Some(OsStr::new("1")), false));
        assert!(!launchd_allowed(None, true));
    }

    #[test]
    fn non_empty_switch_value_turns_launchd_off() {
        assert!(switch_set(Some(OsStr::new("1"))));
        assert!(!switch_set(None));
        assert!(!switch_set(Some(OsStr::new(""))));
    }

    #[test]
    fn guarded_call_without_permission_is_refused() {
        let err = check_call(&["list"], true, false).unwrap_err();
        assert!(err.contains("launchctl is forbidden"), "{err}");
    }

    #[test]
    fn unguarded_or_permitted_calls_pass() {
        assert!(check_call(&["list"], false, false).is_ok());
        assert!(check_call(&["list"], true, true).is_ok());
    }

    #[test]
    fn server_label_is_refused_even_when_permitted() {
        let args = ["bootout", "gui/501/dev.herdr.server.1.2"];
        assert!(check_call(&args, true, true).is_err());
        assert!(check_call(&args, false, false).is_ok());
    }
}
