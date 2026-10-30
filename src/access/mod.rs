// herdr-fork: pure access model (policy types, `decide`, caller classification). Unofficial stopgap; see FORK.md.
//! Per-pane access control for API callers. Pure: no I/O, no wiring. The
//! server's gate calls `decide` with a `CallerClass` from `classify`.

// Wired into the server's gate in a later task.
#[allow(dead_code, unused_imports)]
pub(crate) mod caller;
#[allow(dead_code, unused_imports)]
pub(crate) mod policy;
