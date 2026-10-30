// herdr-fork: classify a socket peer's lineage into a caller class. Unofficial stopgap; see FORK.md.
use crate::platform::{PeerLineage, ProcessIdentity};
use crate::terminal::TerminalId;

use super::policy::{AccessBindingId, CallerClass};

/// Classify a connection. Order: pane match, then the server's own pid, then
/// an incomplete walk (all `Pane`/`Unknown`), then the remote stamp, then
/// `Outside`, upgraded to `Caller` only by a valid token. Anything that can't
/// be shown to be outside is `Unknown`, never `Outside`.
pub(crate) fn classify(
    lineage: &PeerLineage,
    remote: bool,
    panes: &[(TerminalId, ProcessIdentity)],
    server_pid: u32,
    token_match: Option<(String, AccessBindingId)>,
) -> CallerClass {
    let unknown = CallerClass::Unknown {
        remote_hint: remote || lineage.via_ssh,
    };
    for ident in &lineage.chain {
        if let Some((terminal, _)) = panes.iter().find(|(_, shell)| shell == ident) {
            return CallerClass::Pane(terminal.clone());
        }
        if ident.pid == server_pid {
            return unknown;
        }
    }
    // An empty chain proves nothing, complete or not.
    if !lineage.complete || lineage.chain.is_empty() {
        return unknown;
    }
    if remote {
        return CallerClass::Remote;
    }
    let via_ssh = lineage.via_ssh;
    match token_match {
        Some((name, target)) => CallerClass::Caller {
            name,
            target,
            via_ssh,
        },
        None => CallerClass::Outside { via_ssh },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_token: pid as u64 * 10,
        }
    }
    fn lin(pids: &[u32], complete: bool) -> PeerLineage {
        PeerLineage {
            chain: pids.iter().map(|p| id(*p)).collect(),
            complete,
            via_ssh: false,
        }
    }
    fn bid() -> AccessBindingId {
        AccessBindingId::parse(&format!("acc_{}", "b".repeat(32))).unwrap()
    }
    const SERVER: u32 = 500;

    #[test]
    fn unresolved_walk_is_unknown_not_outside() {
        let t = TerminalId::alloc();
        let panes = vec![(t, id(100))];
        let c = classify(&lin(&[200, 300], false), false, &panes, SERVER, None);
        assert_eq!(c, CallerClass::Unknown { remote_hint: false });
        let c = classify(&lin(&[], false), false, &panes, SERVER, None);
        assert_eq!(c, CallerClass::Unknown { remote_hint: false });
        let c = classify(&lin(&[200, 1], true), false, &panes, SERVER, None);
        assert_eq!(c, CallerClass::Outside { via_ssh: false });
    }

    #[test]
    fn empty_complete_chain_is_unknown() {
        let c = classify(&lin(&[], true), false, &[], SERVER, None);
        assert_eq!(c, CallerClass::Unknown { remote_hint: false });
        let c = classify(&lin(&[], true), true, &[], SERVER, None);
        assert_eq!(c, CallerClass::Unknown { remote_hint: true });
    }

    #[test]
    fn unknown_keeps_the_remote_hint() {
        let mut l = lin(&[200], false);
        l.via_ssh = true;
        assert_eq!(
            classify(&l, false, &[], SERVER, None),
            CallerClass::Unknown { remote_hint: true }
        );
        assert_eq!(
            classify(&lin(&[200], false), true, &[], SERVER, None),
            CallerClass::Unknown { remote_hint: true }
        );
        let mut s = lin(&[700, SERVER, 1], true);
        s.via_ssh = true;
        assert_eq!(
            classify(&s, false, &[], SERVER, None),
            CallerClass::Unknown { remote_hint: true }
        );
        assert_eq!(
            classify(&lin(&[200], false), false, &[], SERVER, None),
            CallerClass::Unknown { remote_hint: false }
        );
    }

    #[test]
    fn pane_match_wins_over_later_entries() {
        let t = TerminalId::alloc();
        let panes = vec![(t.clone(), id(100))];
        let c = classify(
            &lin(&[300, 100, SERVER, 1], true),
            false,
            &panes,
            SERVER,
            None,
        );
        assert_eq!(c, CallerClass::Pane(t.clone()));
        // Also on an incomplete walk.
        let c = classify(&lin(&[100, SERVER], false), false, &panes, SERVER, None);
        assert_eq!(c, CallerClass::Pane(t));
    }

    #[test]
    fn pane_match_needs_the_start_token() {
        let t = TerminalId::alloc();
        let panes = vec![(t, id(100))];
        let reused = PeerLineage {
            chain: vec![ProcessIdentity {
                pid: 100,
                start_token: 1,
            }],
            complete: true,
            via_ssh: false,
        };
        let c = classify(&reused, false, &panes, SERVER, None);
        assert_eq!(c, CallerClass::Outside { via_ssh: false });
    }

    #[test]
    fn plugin_child_classifies_as_unknown() {
        let t = TerminalId::alloc();
        let panes = vec![(t, id(100))];
        let c = classify(&lin(&[700, SERVER, 1], true), false, &panes, SERVER, None);
        assert_eq!(c, CallerClass::Unknown { remote_hint: false });
    }

    #[test]
    fn remote_origin_classifies_remote_and_ignores_token() {
        let panes = vec![];
        let c = classify(
            &lin(&[200, 1], true),
            true,
            &panes,
            SERVER,
            Some(("a".into(), bid())),
        );
        assert_eq!(c, CallerClass::Remote);
        let c = classify(&lin(&[200, 1], true), true, &panes, SERVER, None);
        assert_eq!(c, CallerClass::Remote);
    }

    #[test]
    fn stamp_from_pane_stays_pane_and_ignores_token() {
        let t = TerminalId::alloc();
        let panes = vec![(t.clone(), id(100))];
        let c = classify(
            &lin(&[100, 1], true),
            true,
            &panes,
            SERVER,
            Some(("a".into(), bid())),
        );
        assert_eq!(c, CallerClass::Pane(t));
    }

    #[test]
    fn caller_token_ignored_from_pane_and_unknown() {
        let t = TerminalId::alloc();
        let panes = vec![(t.clone(), id(100))];
        let tok = || Some(("a".to_string(), bid()));
        assert_eq!(
            classify(&lin(&[100, 1], true), false, &panes, SERVER, tok()),
            CallerClass::Pane(t)
        );
        assert_eq!(
            classify(&lin(&[SERVER, 1], true), false, &panes, SERVER, tok()),
            CallerClass::Unknown { remote_hint: false }
        );
        assert_eq!(
            classify(&lin(&[200], false), false, &panes, SERVER, tok()),
            CallerClass::Unknown { remote_hint: false }
        );
    }

    #[test]
    fn token_from_outside_becomes_caller() {
        let c = classify(
            &lin(&[200, 1], true),
            false,
            &[],
            SERVER,
            Some(("a".into(), bid())),
        );
        assert_eq!(
            c,
            CallerClass::Caller {
                name: "a".into(),
                target: bid(),
                via_ssh: false
            }
        );
    }

    #[test]
    fn via_ssh_token_from_outside_becomes_caller() {
        let mut l = lin(&[200, 1], true);
        l.via_ssh = true;
        let c = classify(&l, false, &[], SERVER, Some(("a".into(), bid())));
        assert_eq!(
            c,
            CallerClass::Caller {
                name: "a".into(),
                target: bid(),
                via_ssh: true
            }
        );
        assert_eq!(
            classify(&l, false, &[], SERVER, None),
            CallerClass::Outside { via_ssh: true }
        );
    }
}
