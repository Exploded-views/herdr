// herdr-fork: access policy types and the `decide` function; narrow dead-code allows remain only for the access.* methods that have not landed. Unofficial stopgap; see FORK.md.
use std::fmt;
use std::io;

use crate::terminal::TerminalId;

/// Who a pane accepts input from. Text form: `none`, `outside`, `caller:<name>`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) enum Principal {
    None,
    Outside,
    Caller(String),
}

impl From<Principal> for String {
    fn from(p: Principal) -> String {
        match p {
            Principal::None => "none".to_string(),
            Principal::Outside => "outside".to_string(),
            Principal::Caller(n) => format!("caller:{n}"),
        }
    }
}

pub(crate) fn valid_caller_name(name: &str) -> bool {
    let mut chars = name.chars();
    let first_ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit());
    first_ok
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

impl TryFrom<String> for Principal {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        match value.as_str() {
            "none" => Ok(Principal::None),
            "outside" => Ok(Principal::Outside),
            "any" => Err("\"any\" is only valid as the sole accept_from entry".to_string()),
            other => match other.strip_prefix("caller:") {
                Some(name) if valid_caller_name(name) => Ok(Principal::Caller(name.to_string())),
                Some(_) => Err(format!("invalid caller name in {other:?}")),
                None => Err(format!("unknown accept_from entry {other:?}")),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcceptFrom {
    Any,
    Only(Vec<Principal>),
}

/// Parse an explicit `accept_from` list. `any` is valid only alone; globs,
/// empty lists and unknown entries are refused.
pub(crate) fn parse_accept_from(values: &[String]) -> Result<AcceptFrom, String> {
    if values.is_empty() {
        return Err("accept_from is empty; use \"none\" to accept nothing".to_string());
    }
    if values.iter().any(|v| v == "any") {
        return if values.len() == 1 {
            Ok(AcceptFrom::Any)
        } else {
            Err("\"any\" cannot be combined with other accept_from entries".to_string())
        };
    }
    let mut out = Vec::with_capacity(values.len());
    for v in values {
        out.push(Principal::try_from(v.clone())?);
    }
    Ok(AcceptFrom::Only(out))
}

/// True when `explicit` would let in someone the pane's current policy does
/// not. `current == None` means no policy (open), which nothing is looser than.
pub(crate) fn is_looser(explicit: &AcceptFrom, current: Option<&[Principal]>) -> bool {
    let Some(current) = current else {
        return false;
    };
    match explicit {
        AcceptFrom::Any => true,
        AcceptFrom::Only(list) => list
            .iter()
            .any(|p| *p != Principal::None && !current.contains(p)),
    }
}

/// `acc_` followed by 32 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct AccessBindingId(String);

impl From<AccessBindingId> for String {
    fn from(id: AccessBindingId) -> String {
        id.0
    }
}

impl TryFrom<String> for AccessBindingId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        Self::parse(&value).ok_or_else(|| format!("invalid access binding id {value:?}"))
    }
}

impl AccessBindingId {
    pub(crate) fn mint() -> io::Result<Self> {
        let secret = crate::platform::random_secret()?;
        let mut s = String::from("acc_");
        for b in &secret[..16] {
            s.push_str(&format!("{b:02x}"));
        }
        Ok(Self(s))
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        let hex = text.strip_prefix("acc_")?;
        let ok = hex.len() == 32 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        ok.then(|| Self(text.to_string()))
    }

    // Read by the caller registry and `access.*` output (Tasks 8 and 9); tests read it now.
    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AccessBindingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct AccessPolicy {
    pub binding: AccessBindingId,
    pub accept_from: Vec<Principal>,
}

/// Startup-resolved `[access]` sections: the sticky default for newly
/// detected agent panes and the named sections. Read once; reload leaves it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct AccessDefaults {
    pub default: Option<Vec<Principal>>,
    pub by_name: std::collections::BTreeMap<String, Vec<Principal>>,
}

/// `[access] remote_owner_tui`: whether the owner's remote TUI may still type
/// into protected panes when remote input is bridge-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RemoteOwnerTui {
    #[default]
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CallerClass {
    Pane(TerminalId),
    Outside {
        via_ssh: bool,
    },
    /// Not shown to be outside. `remote_hint` is set when a remote stamp or an
    /// ssh-server ancestor was seen on the way; it only ever restricts.
    Unknown {
        remote_hint: bool,
    },
    Remote,
    Caller {
        name: String,
        target: AccessBindingId,
        via_ssh: bool,
    },
}

pub(crate) enum GateTarget<'a> {
    Open,
    Input {
        label: String,
        terminal: &'a TerminalId,
        policy: Option<&'a AccessPolicy>,
    },
    /// `agent.start` into a pane.
    PaneControl {
        label: String,
        terminal: &'a TerminalId,
        policy: Option<&'a AccessPolicy>,
    },
    /// `access.pane.set`, `access.caller.*`.
    // Built by `gate_target` for an `agent.start` that loosens a policy.
    AccessAdmin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RemoteInput {
    #[default]
    Any,
    BridgeOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RemoteSettings {
    pub input: RemoteInput,
    pub bridge_agent: Option<String>,
    pub bridge_binding: Option<AccessBindingId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AccessDenied {
    Input {
        label: String,
        from: String,
    },
    Control {
        label: String,
        from: String,
    },
    Remote {
        label: String,
        bridge_agent: Option<String>,
        bridge_binding: Option<AccessBindingId>,
    },
}

const TAIL: &str = "This is deliberate policy. Do not retry with another herdr command, attach, tmux, or any other route. Use your own messaging channel (e.g. SendMessage) or report to your operator.";

impl AccessDenied {
    pub(crate) fn message(&self) -> String {
        match self {
            AccessDenied::Input { label, from } => {
                format!("access_denied: pane {label} does not accept input from {from}. {TAIL}")
            }
            AccessDenied::Control { label, from } => format!(
                "access_denied: pane {label} does not accept control requests from {from}. {TAIL}"
            ),
            AccessDenied::Remote {
                label,
                bridge_agent,
                bridge_binding,
            } => {
                let who = match (bridge_agent, bridge_binding) {
                    (Some(a), Some(b)) => format!("({a} {b})"),
                    (Some(a), None) => format!("({a})"),
                    (None, _) => "(this node's bridge)".to_string(),
                };
                format!(
                    "access_denied: pane {label} does not accept input from another host. This is deliberate policy. Send it to this node's bridge {who} through your own bridge. Do not retry with another herdr command, attach, ssh, tmux, or any other route."
                )
            }
        }
    }
}

/// Does one principal admit this caller on a pane bound to `binding`? `None`
/// admits nobody; a named caller is admitted only with this pane's binding.
fn principal_admits(
    principal: &Principal,
    caller: &CallerClass,
    binding: &AccessBindingId,
) -> bool {
    match principal {
        Principal::None => false,
        Principal::Outside => matches!(caller, CallerClass::Outside { .. }),
        Principal::Caller(n) => matches!(
            caller,
            CallerClass::Caller { name, target, .. } if name == n && target == binding
        ),
    }
}

fn from_text(caller: &CallerClass) -> String {
    match caller {
        CallerClass::Pane(t) => format!("pane:{t}"),
        CallerClass::Outside { .. } => "outside".to_string(),
        CallerClass::Unknown { .. } => "unknown".to_string(),
        CallerClass::Remote => "remote".to_string(),
        CallerClass::Caller { name, .. } => format!("caller:{name}"),
    }
}

fn input_denied(label: &str, caller: &CallerClass) -> AccessDenied {
    AccessDenied::Input {
        label: label.to_string(),
        from: from_text(caller),
    }
}

fn control_denied(label: &str, caller: &CallerClass) -> AccessDenied {
    AccessDenied::Control {
        label: label.to_string(),
        from: from_text(caller),
    }
}

/// Allow or deny `caller` against `target`. Default is deny; every `Ok` is an
/// explicit branch below.
pub(crate) fn decide(
    target: &GateTarget,
    caller: &CallerClass,
    remote: &RemoteSettings,
) -> Result<(), AccessDenied> {
    match target {
        // Rule 1.
        GateTarget::Open => Ok(()),
        GateTarget::Input {
            label,
            policy,
            terminal: _,
        } => {
            // Rule 0: remote input. Only denies; never grants.
            if remote.input == RemoteInput::BridgeOnly {
                let remote_origin = matches!(
                    caller,
                    CallerClass::Remote
                        | CallerClass::Unknown { remote_hint: true }
                        | CallerClass::Outside { via_ssh: true }
                        | CallerClass::Caller { via_ssh: true, .. }
                );
                let on_own_binding = matches!(
                    (caller, policy),
                    (CallerClass::Caller { target, .. }, Some(p)) if p.binding == *target
                );
                if remote_origin && !on_own_binding {
                    return Err(AccessDenied::Remote {
                        label: label.clone(),
                        bridge_agent: remote.bridge_agent.clone(),
                        bridge_binding: remote.bridge_binding.clone(),
                    });
                }
            }
            // Rule 2: a caller token is confined to its own binding.
            if let CallerClass::Caller { target, .. } = caller {
                if !matches!(policy, Some(p) if p.binding == *target) {
                    return Err(input_denied(label, caller));
                }
            }
            // Rule 3.
            let Some(p) = policy else {
                return Ok(());
            };
            let matched = p
                .accept_from
                .iter()
                .any(|principal| principal_admits(principal, caller, &p.binding));
            if matched {
                Ok(())
            } else {
                Err(input_denied(label, caller))
            }
        }
        GateTarget::PaneControl {
            label,
            policy,
            terminal: _,
        } => {
            // Rule 2.
            if let CallerClass::Caller { target, .. } = caller {
                return if matches!(policy, Some(p) if p.binding == *target) {
                    Ok(())
                } else {
                    Err(control_denied(label, caller))
                };
            }
            // Rule 4.
            if policy.is_none() || matches!(caller, CallerClass::Outside { .. }) {
                Ok(())
            } else {
                Err(control_denied(label, caller))
            }
        }
        // Rule 5.
        GateTarget::AccessAdmin => match caller {
            CallerClass::Outside { .. } => Ok(()),
            _ => Err(control_denied("server", caller)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bid(n: u8) -> AccessBindingId {
        AccessBindingId::parse(&format!("acc_{:032x}", n)).unwrap()
    }
    fn pol(n: u8, from: Vec<Principal>) -> AccessPolicy {
        AccessPolicy {
            binding: bid(n),
            accept_from: from,
        }
    }
    fn outside() -> CallerClass {
        CallerClass::Outside { via_ssh: false }
    }
    fn caller(name: &str, n: u8) -> CallerClass {
        CallerClass::Caller {
            name: name.to_string(),
            target: bid(n),
            via_ssh: false,
        }
    }
    fn input<'a>(t: &'a TerminalId, p: Option<&'a AccessPolicy>) -> GateTarget<'a> {
        GateTarget::Input {
            label: "p1".into(),
            terminal: t,
            policy: p,
        }
    }
    fn control<'a>(t: &'a TerminalId, p: Option<&'a AccessPolicy>) -> GateTarget<'a> {
        GateTarget::PaneControl {
            label: "p1".into(),
            terminal: t,
            policy: p,
        }
    }
    fn d(t: &GateTarget, c: &CallerClass) -> Result<(), AccessDenied> {
        decide(t, c, &RemoteSettings::default())
    }
    fn all_classes(t: &TerminalId) -> Vec<CallerClass> {
        vec![
            CallerClass::Pane(t.clone()),
            outside(),
            CallerClass::Unknown { remote_hint: false },
            CallerClass::Remote,
            caller("a", 1),
        ]
    }

    #[test]
    fn open_target_always_allowed() {
        let t = TerminalId::alloc();
        for c in all_classes(&t) {
            assert!(d(&GateTarget::Open, &c).is_ok());
        }
    }

    #[test]
    fn input_open_without_policy() {
        let t = TerminalId::alloc();
        for c in [
            CallerClass::Pane(t.clone()),
            outside(),
            CallerClass::Unknown { remote_hint: false },
            CallerClass::Remote,
        ] {
            assert!(d(&input(&t, None), &c).is_ok(), "{c:?}");
        }
    }

    #[test]
    fn input_none_denies_everyone_including_outside() {
        let t = TerminalId::alloc();
        let p = pol(1, vec![Principal::None]);
        for c in all_classes(&t) {
            assert!(d(&input(&t, Some(&p)), &c).is_err(), "{c:?}");
        }
        let empty = pol(1, vec![]);
        assert!(d(&input(&t, Some(&empty)), &outside()).is_err());
    }

    #[test]
    fn input_outside_principal_allows_outside_only() {
        let t = TerminalId::alloc();
        let p = pol(1, vec![Principal::Outside]);
        let tg = input(&t, Some(&p));
        assert!(d(&tg, &outside()).is_ok());
        assert!(d(&tg, &CallerClass::Pane(t.clone())).is_err());
        assert!(d(&tg, &CallerClass::Unknown { remote_hint: false }).is_err());
        assert!(d(&tg, &CallerClass::Remote).is_err());
        assert!(d(&tg, &caller("a", 1)).is_err());
    }

    #[test]
    fn input_caller_principal_requires_matching_binding() {
        let t = TerminalId::alloc();
        let p = pol(1, vec![Principal::Caller("a".into())]);
        let tg = input(&t, Some(&p));
        assert!(d(&tg, &caller("a", 1)).is_ok());
        assert!(d(&tg, &caller("a", 2)).is_err());
        assert!(d(&tg, &caller("b", 1)).is_err());
        assert!(d(&tg, &outside()).is_err());
    }

    #[test]
    fn caller_token_confined_to_its_binding() {
        let t = TerminalId::alloc();
        let c = caller("a", 1);
        assert!(d(&input(&t, None), &c).is_err());
        assert!(d(&control(&t, None), &c).is_err());
        let other = pol(2, vec![Principal::Caller("a".into()), Principal::Outside]);
        assert!(d(&input(&t, Some(&other)), &c).is_err());
        assert!(d(&control(&t, Some(&other)), &c).is_err());
        let own = pol(1, vec![Principal::Caller("a".into())]);
        assert!(d(&input(&t, Some(&own)), &c).is_ok());
        assert!(d(&control(&t, Some(&own)), &c).is_ok());
    }

    #[test]
    fn remote_and_unknown_never_match() {
        let t = TerminalId::alloc();
        let p = pol(
            1,
            vec![
                Principal::Outside,
                Principal::Caller("a".into()),
                Principal::None,
            ],
        );
        for c in [
            CallerClass::Remote,
            CallerClass::Unknown { remote_hint: false },
        ] {
            assert!(d(&input(&t, Some(&p)), &c).is_err());
            assert!(d(&control(&t, Some(&p)), &c).is_err());
        }
        assert!(d(&input(&t, Some(&p)), &CallerClass::Pane(t.clone())).is_err());
    }

    #[test]
    fn pane_control_allows_outside_and_bound_caller_only() {
        let t = TerminalId::alloc();
        let p = pol(1, vec![Principal::None]);
        let tg = control(&t, Some(&p));
        assert!(d(&tg, &outside()).is_ok());
        assert!(d(&tg, &caller("a", 1)).is_ok());
        assert!(d(&tg, &caller("a", 2)).is_err());
        assert!(d(&tg, &CallerClass::Pane(t.clone())).is_err());
        assert!(d(&tg, &CallerClass::Unknown { remote_hint: false }).is_err());
        assert!(d(&tg, &CallerClass::Remote).is_err());
        for c in [
            CallerClass::Pane(t.clone()),
            outside(),
            CallerClass::Unknown { remote_hint: false },
            CallerClass::Remote,
        ] {
            assert!(d(&control(&t, None), &c).is_ok());
        }
    }

    #[test]
    fn access_admin_outside_only() {
        let t = TerminalId::alloc();
        assert!(d(&GateTarget::AccessAdmin, &outside()).is_ok());
        for c in [
            CallerClass::Pane(t.clone()),
            CallerClass::Unknown { remote_hint: false },
            CallerClass::Remote,
            caller("a", 1),
        ] {
            let e = d(&GateTarget::AccessAdmin, &c).unwrap_err();
            assert!(
                e.message().starts_with(
                    "access_denied: pane server does not accept control requests from "
                ),
                "{}",
                e.message()
            );
        }
    }

    #[test]
    fn denial_messages_match_contract() {
        let t = TerminalId::alloc();
        let e = AccessDenied::Input {
            label: "w1-2".into(),
            from: from_text(&caller("a", 1)),
        };
        assert_eq!(
            e.message(),
            "access_denied: pane w1-2 does not accept input from caller:a. This is deliberate policy. Do not retry with another herdr command, attach, tmux, or any other route. Use your own messaging channel (e.g. SendMessage) or report to your operator."
        );
        let p = pol(1, vec![Principal::None]);
        let e = d(
            &control(&t, Some(&p)),
            &CallerClass::Unknown { remote_hint: false },
        )
        .unwrap_err();
        assert_eq!(
            e.message(),
            "access_denied: pane p1 does not accept control requests from unknown. This is deliberate policy. Do not retry with another herdr command, attach, tmux, or any other route. Use your own messaging channel (e.g. SendMessage) or report to your operator."
        );
        let e = d(&input(&t, Some(&p)), &CallerClass::Remote).unwrap_err();
        assert!(e.message().contains("input from remote."));
        let e = d(&input(&t, Some(&p)), &CallerClass::Pane(t.clone())).unwrap_err();
        assert!(e.message().contains(&format!("input from pane:{t}.")));
        let e = d(&input(&t, Some(&p)), &outside()).unwrap_err();
        assert!(e.message().contains("input from outside."));
    }

    #[test]
    fn parse_accept_from_rejects_any_mixed_and_globs() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_accept_from(&s(&["any"])), Ok(AcceptFrom::Any));
        assert!(parse_accept_from(&s(&["any", "outside"])).is_err());
        assert!(parse_accept_from(&s(&["outside", "any"])).is_err());
        assert!(parse_accept_from(&s(&["caller:*"])).is_err());
        assert!(parse_accept_from(&s(&["caller:a?"])).is_err());
        assert!(parse_accept_from(&s(&["caller:"])).is_err());
        for bad in [
            "caller:..x",
            "caller:.x",
            "caller:X",
            "caller:a.b",
            "caller:-a",
            "caller:_a",
        ] {
            assert!(parse_accept_from(&s(&[bad])).is_err(), "{bad}");
        }
        let long65 = format!("caller:{}", "a".repeat(65));
        assert!(parse_accept_from(&s(&[long65.as_str()])).is_err());
        let long64 = format!("caller:{}", "a".repeat(64));
        assert!(parse_accept_from(&s(&[long64.as_str()])).is_ok());
        assert!(parse_accept_from(&s(&["caller:0a_b-c"])).is_ok());
        assert!(parse_accept_from(&s(&["*"])).is_err());
        assert!(parse_accept_from(&s(&["pane:x"])).is_err());
        assert!(parse_accept_from(&[]).is_err());
        assert_eq!(
            parse_accept_from(&s(&["none", "outside", "caller:a-b"])),
            Ok(AcceptFrom::Only(vec![
                Principal::None,
                Principal::Outside,
                Principal::Caller("a-b".into())
            ]))
        );
    }

    #[test]
    fn is_looser_cases() {
        let out = vec![Principal::Outside];
        let a = Principal::Caller("a".into());
        assert!(!is_looser(&AcceptFrom::Any, None));
        assert!(!is_looser(
            &AcceptFrom::Only(vec![Principal::Outside]),
            None
        ));
        assert!(is_looser(&AcceptFrom::Any, Some(&out)));
        assert!(!is_looser(
            &AcceptFrom::Only(vec![Principal::Outside]),
            Some(&out)
        ));
        assert!(!is_looser(
            &AcceptFrom::Only(vec![Principal::None]),
            Some(&out)
        ));
        assert!(is_looser(&AcceptFrom::Only(vec![a.clone()]), Some(&out)));
        assert!(is_looser(
            &AcceptFrom::Only(vec![Principal::Outside, a.clone()]),
            Some(&out)
        ));
        assert!(!is_looser(
            &AcceptFrom::Only(vec![Principal::Outside]),
            Some(&[Principal::Outside, a])
        ));
        assert!(is_looser(
            &AcceptFrom::Only(vec![Principal::Outside]),
            Some(&[])
        ));
    }

    #[test]
    fn binding_id_parse_and_mint() {
        assert!(AccessBindingId::parse(&format!("acc_{}", "a".repeat(32))).is_some());
        assert!(AccessBindingId::parse(&format!("acc_{}", "A".repeat(32))).is_none());
        assert!(AccessBindingId::parse(&format!("acc_{}", "a".repeat(31))).is_none());
        assert!(AccessBindingId::parse(&format!("acc_{}", "a".repeat(33))).is_none());
        assert!(AccessBindingId::parse(&format!("acc_{}", "g".repeat(32))).is_none());
        assert!(AccessBindingId::parse(&"a".repeat(36)).is_none());
        let m = AccessBindingId::mint().unwrap();
        assert_eq!(AccessBindingId::parse(m.as_str()), Some(m));
    }

    #[test]
    fn principal_serde_round_trip() {
        let ps = vec![
            Principal::None,
            Principal::Outside,
            Principal::Caller("x".into()),
        ];
        let j = serde_json::to_string(&ps).unwrap();
        assert_eq!(j, r#"["none","outside","caller:x"]"#);
        assert_eq!(serde_json::from_str::<Vec<Principal>>(&j).unwrap(), ps);
        assert!(serde_json::from_str::<Principal>(r#""any""#).is_err());
    }

    fn bridge_only() -> RemoteSettings {
        RemoteSettings {
            input: RemoteInput::BridgeOnly,
            bridge_agent: Some("bridge".into()),
            bridge_binding: Some(bid(9)),
        }
    }

    #[test]
    fn remote_input_bridge_only_denies_open_pane() {
        let t = TerminalId::alloc();
        let rs = bridge_only();
        let via = CallerClass::Outside { via_ssh: true };
        for c in [CallerClass::Remote, via] {
            let e = decide(&input(&t, None), &c, &rs).unwrap_err();
            assert!(matches!(e, AccessDenied::Remote { .. }), "{c:?}");
        }
        let p = pol(1, vec![Principal::Outside]);
        assert!(decide(
            &input(&t, Some(&p)),
            &CallerClass::Outside { via_ssh: true },
            &rs
        )
        .is_err());
        // Local outside is unaffected.
        assert!(decide(&input(&t, None), &outside(), &rs).is_ok());
        // Rule 0 is for Input only.
        assert!(decide(&control(&t, None), &CallerClass::Remote, &rs).is_ok());
    }

    #[test]
    fn remote_input_bridge_only_allows_confined_caller_on_bridge_only() {
        let t = TerminalId::alloc();
        let rs = bridge_only();
        let p = pol(9, vec![Principal::Caller("bridge".into())]);
        let c = CallerClass::Caller {
            name: "bridge".into(),
            target: bid(9),
            via_ssh: true,
        };
        assert!(decide(&input(&t, Some(&p)), &c, &rs).is_ok());
        // Same token on another pane: remote refusal.
        let other = pol(8, vec![Principal::Caller("bridge".into())]);
        assert!(matches!(
            decide(&input(&t, Some(&other)), &c, &rs),
            Err(AccessDenied::Remote { .. })
        ));
        assert!(matches!(
            decide(&input(&t, None), &c, &rs),
            Err(AccessDenied::Remote { .. })
        ));
        // A confined caller whose policy does not list it is still denied by rule 3.
        let unlisted = pol(9, vec![Principal::Outside]);
        assert!(decide(&input(&t, Some(&unlisted)), &c, &rs).is_err());
    }

    #[test]
    fn remote_input_any_keeps_rev3_behaviour() {
        let t = TerminalId::alloc();
        let rs = RemoteSettings::default();
        assert_eq!(rs.input, RemoteInput::Any);
        // R1: remote to an open pane is allowed; R2: ssh outside likewise.
        assert!(decide(&input(&t, None), &CallerClass::Remote, &rs).is_ok());
        assert!(decide(
            &input(&t, None),
            &CallerClass::Outside { via_ssh: true },
            &rs
        )
        .is_ok());
        // R3: remote to a protected pane is a normal denial, not the remote text.
        let p = pol(1, vec![Principal::Outside]);
        let e = decide(&input(&t, Some(&p)), &CallerClass::Remote, &rs).unwrap_err();
        assert!(matches!(e, AccessDenied::Input { .. }));
        // R4: ssh outside matches the outside principal.
        assert!(decide(
            &input(&t, Some(&p)),
            &CallerClass::Outside { via_ssh: true },
            &rs
        )
        .is_ok());
    }

    #[test]
    fn via_ssh_ignored_by_rules_three_to_five() {
        let t = TerminalId::alloc();
        let rs = RemoteSettings::default();
        let via = CallerClass::Outside { via_ssh: true };
        let p = pol(1, vec![Principal::None]);
        assert!(decide(&input(&t, Some(&p)), &via, &rs).is_err());
        assert!(decide(&control(&t, Some(&p)), &via, &rs).is_ok());
        assert!(decide(&GateTarget::AccessAdmin, &via, &rs).is_ok());
        let po = pol(1, vec![Principal::Outside]);
        assert!(decide(&input(&t, Some(&po)), &via, &rs).is_ok());
        let cv = CallerClass::Caller {
            name: "a".into(),
            target: bid(1),
            via_ssh: true,
        };
        let pc = pol(1, vec![Principal::Caller("a".into())]);
        assert!(decide(&input(&t, Some(&pc)), &cv, &rs).is_ok());
        assert!(decide(&GateTarget::AccessAdmin, &cv, &rs).is_err());
    }

    #[test]
    fn remote_denial_names_bridge_without_host() {
        let mk = |a: Option<&str>, b: Option<AccessBindingId>| AccessDenied::Remote {
            label: "w1-2".into(),
            bridge_agent: a.map(String::from),
            bridge_binding: b,
        };
        let b = bid(9);
        assert_eq!(
            mk(Some("bridge"), Some(b.clone())).message(),
            format!("access_denied: pane w1-2 does not accept input from another host. This is deliberate policy. Send it to this node's bridge (bridge {b}) through your own bridge. Do not retry with another herdr command, attach, ssh, tmux, or any other route.")
        );
        assert_eq!(
            mk(Some("bridge"), None).message(),
            "access_denied: pane w1-2 does not accept input from another host. This is deliberate policy. Send it to this node's bridge (bridge) through your own bridge. Do not retry with another herdr command, attach, ssh, tmux, or any other route."
        );
        assert_eq!(
            mk(None, None).message(),
            "access_denied: pane w1-2 does not accept input from another host. This is deliberate policy. Send it to this node's bridge (this node's bridge) through your own bridge. Do not retry with another herdr command, attach, ssh, tmux, or any other route."
        );
        assert_eq!(mk(None, Some(b)).message(), mk(None, None).message());
    }

    #[test]
    fn machine_input_refused_on_bridge_when_bridge_only() {
        // C2: a remote machine (stamp) and an ssh-reached outside shell are
        // both refused on a pane bound to the bridge's own policy.
        let t = TerminalId::alloc();
        let rs = bridge_only();
        let p = pol(
            9,
            vec![Principal::Outside, Principal::Caller("bridge".into())],
        );
        for c in [CallerClass::Remote, CallerClass::Outside { via_ssh: true }] {
            let e = decide(&input(&t, Some(&p)), &c, &rs).unwrap_err();
            assert!(e.message().contains("another host"), "{c:?}");
            assert!(!e.message().contains("caller:"));
        }
    }

    #[test]
    fn principal_admits_cases() {
        let t = TerminalId::alloc();
        let b = bid(1);
        let a = Principal::Caller("a".into());
        assert!(principal_admits(&a, &caller("a", 1), &b));
        assert!(!principal_admits(&a, &caller("a", 2), &b));
        assert!(!principal_admits(&a, &caller("b", 1), &b));
        assert!(!principal_admits(&a, &outside(), &b));
        assert!(principal_admits(&Principal::Outside, &outside(), &b));
        assert!(!principal_admits(&Principal::Outside, &caller("a", 1), &b));
        for c in all_classes(&t) {
            assert!(!principal_admits(&Principal::None, &c, &b), "{c:?}");
        }
        for c in [
            CallerClass::Remote,
            CallerClass::Unknown { remote_hint: true },
            CallerClass::Unknown { remote_hint: false },
            CallerClass::Pane(t.clone()),
        ] {
            assert!(!principal_admits(&a, &c, &b));
            assert!(!principal_admits(&Principal::Outside, &c, &b));
        }
    }

    #[test]
    fn bridge_only_refuses_incomplete_stamped_walk() {
        let t = TerminalId::alloc();
        let rs = bridge_only();
        let hinted = CallerClass::Unknown { remote_hint: true };
        let e = decide(&input(&t, None), &hinted, &rs).unwrap_err();
        assert!(matches!(e, AccessDenied::Remote { .. }));
        let p = pol(1, vec![Principal::Outside]);
        assert!(matches!(
            decide(&input(&t, Some(&p)), &hinted, &rs),
            Err(AccessDenied::Remote { .. })
        ));
        // Rule 0 is for Input only.
        assert!(decide(&control(&t, None), &hinted, &rs).is_ok());
    }

    #[test]
    fn bridge_only_refuses_incomplete_ssh_walk() {
        // classify() sets remote_hint from via_ssh on an incomplete walk.
        let lineage = crate::platform::PeerLineage {
            chain: vec![crate::platform::ProcessIdentity {
                pid: 200,
                start_token: 1,
            }],
            complete: false,
            via_ssh: true,
        };
        let c = crate::access::caller::classify(&lineage, false, &[], 500, None);
        assert_eq!(c, CallerClass::Unknown { remote_hint: true });
        let t = TerminalId::alloc();
        assert!(matches!(
            decide(&input(&t, None), &c, &bridge_only()),
            Err(AccessDenied::Remote { .. })
        ));
    }

    #[test]
    fn bridge_only_keeps_local_unknown_on_open_pane() {
        let t = TerminalId::alloc();
        let local = CallerClass::Unknown { remote_hint: false };
        assert!(decide(&input(&t, None), &local, &bridge_only()).is_ok());
        let p = pol(1, vec![Principal::Outside]);
        let e = decide(&input(&t, Some(&p)), &local, &bridge_only()).unwrap_err();
        assert!(matches!(e, AccessDenied::Input { .. }));
    }
}
