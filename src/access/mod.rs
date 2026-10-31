// herdr-fork: pure access model (policy types, `decide`, caller classification) and the hashed-token caller registry. Unofficial stopgap; see FORK.md.
//! Per-pane access control for API callers. Pure: no I/O, no wiring. The
//! gate in `app::access_gate` calls `decide` with a `CallerClass` from `classify`.

pub(crate) mod caller;
pub(crate) mod policy;
pub(crate) mod registry;
