// herdr-fork: fork-only. Keeps the test harness off the real launchd domain. See FORK.md.
//! The harness must set HERDR_NO_LAUNCHD on every herdr process it builds, so a test never
//! reaches the user's real launchd domain. These tests only inspect the built commands.

pub mod support;

use std::ffi::OsStr;

#[test]
fn shared_command_helper_sets_the_no_launchd_switch() {
    let command = support::herdr_command();
    let value = command
        .get_envs()
        .find(|(key, _)| *key == OsStr::new("HERDR_NO_LAUNCHD"))
        .and_then(|(_, value)| value);
    assert_eq!(value, Some(OsStr::new("1")));
}

#[test]
fn pty_isolation_helper_sets_the_no_launchd_switch() {
    let mut command = portable_pty::CommandBuilder::new("herdr");
    support::isolate_herdr_test_process(&mut command);
    assert_eq!(
        command.get_env("HERDR_NO_LAUNCHD"),
        Some(OsStr::new("1"))
    );
}
