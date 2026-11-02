// herdr-fork: params and results for the `access.caller.*`, `access.pane.*` (including the pane's input mode) and `access.log` methods. Unofficial stopgap; see FORK.md.
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
pub struct AccessPaneSetParams {
    /// Public id of the pane whose policy is set.
    pub pane_id: String,
    /// Who the pane accepts input from: `none`, `outside`, `caller:<name>`, or
    /// `any` alone, which removes the policy and opens the pane on purpose. Omit it to
    /// leave the policy as it is (when setting `input_mode` alone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_from: Option<Vec<String>>,
    /// Where the pane's input may come from: `any` (default) or `human-only`, which
    /// refuses input sent through the herdr API and client input from anything but an
    /// outside client. `flag` is not available in v1. Omit it to leave the mode as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AccessPaneGetParams {
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AccessLogParams {
    /// How many entries to return, newest first. Default 100, at most 4096.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// One entry of the input-origin log. There is no field for the input itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AccessInputEvent {
    /// Unix milliseconds of the first input in the entry.
    pub at_ms: u64,
    /// Public pane id at the time.
    pub pane: String,
    pub terminal: String,
    /// `api` or `client`.
    pub path: String,
    /// Method name, or `shell.pane_input`, `shell.clipboard_image`, `shell.popup_input`,
    /// `attach.input`, `attach.mouse`.
    pub route: String,
    /// `pane`, `outside`, `outside_ssh`, `remote`, `caller` or `unknown`.
    pub class: String,
    /// Caller name, or the sending pane's public id for a pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    /// `api:<connection>`, `shell:<client>` or `attach:<client>`.
    pub client: String,
    /// `local`, `ssh` or `remote-stamp`.
    pub host: String,
    /// Payload bytes, summed over the entry.
    pub bytes: u64,
    /// `allow` or `deny`.
    pub decision: String,
    /// Repeats merged into the entry.
    pub count: u32,
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
/// minted; its `Debug` never shows it. There is no derived `==`: the only
/// equality is the constant-time one below, and tokens are matched through
/// the registry's hashes, never compared as text.
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct CallerToken(String);

impl CallerToken {
    pub(crate) fn new(token: String) -> Self {
        Self(token)
    }

    /// The token text. Callers must not log it.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

// Needed because `ResponseResult` derives `PartialEq`. Looks at every byte.
impl PartialEq for CallerToken {
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (self.0.as_bytes(), other.0.as_bytes());
        let mut diff = a.len() ^ b.len();
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= usize::from(x ^ y);
        }
        std::hint::black_box(diff) == 0
    }
}

impl std::fmt::Debug for CallerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CallerToken(<redacted>)")
    }
}
