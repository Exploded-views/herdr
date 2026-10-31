// herdr-fork: params and results for the `access.caller.*` methods. Unofficial stopgap; see FORK.md.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AccessCallerAddParams {
    /// Caller name: `[a-z0-9][a-z0-9_-]{0,63}`.
    pub name: String,
    /// Public id of the pane the caller may send input to. The pane must hold an access policy.
    pub target_pane: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AccessCallerNameParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AccessCallerRetargetParams {
    pub name: String,
    /// Public id of the pane the caller is moved to. The pane must hold an access policy.
    pub target_pane: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AccessCallerInfo {
    pub name: String,
    /// The access binding id (`acc_...`) the caller is bound to.
    pub binding: String,
    /// Public id of the pane that holds that binding now, if it is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

/// A caller token. It appears on the wire in exactly one result, when it is
/// minted; its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct CallerToken(String);

impl CallerToken {
    pub fn new(token: String) -> Self {
        Self(token)
    }

    /// The token text. Callers must not log it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for CallerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CallerToken(<redacted>)")
    }
}
