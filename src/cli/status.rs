// herdr-fork: `status server` shows the server's binary, fork tag, commit and sha256; the stale-server check also compares fork tags. Unofficial stopgap; see FORK.md.
use serde::Serialize;

use crate::api;
use crate::api::client::ApiClientError;

pub(super) fn run_status_command(args: &[String]) -> std::io::Result<i32> {
    let Some((scope, json)) = parse_status_args(args) else {
        return Ok(2);
    };

    match scope {
        StatusScope::Full => print_full_status(json),
        StatusScope::Server => print_server_status(json),
        StatusScope::Client => {
            print_client_status(json)?;
            Ok(0)
        }
        StatusScope::Help => {
            print_status_help();
            Ok(0)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusScope {
    Full,
    Server,
    Client,
    Help,
}

fn parse_status_args(args: &[String]) -> Option<(StatusScope, bool)> {
    match args.first().map(|arg| arg.as_str()) {
        None => Some((StatusScope::Full, false)),
        Some("--json") if args.len() == 1 => Some((StatusScope::Full, true)),
        Some("server") => {
            parse_status_scope_args(args, StatusScope::Server, "herdr status server [--json]")
        }
        Some("client") => {
            parse_status_scope_args(args, StatusScope::Client, "herdr status client [--json]")
        }
        Some("help" | "--help" | "-h") => {
            if args.len() > 1 {
                print_status_help();
                return None;
            }
            Some((StatusScope::Help, false))
        }
        Some(_) => {
            print_status_help();
            None
        }
    }
}

fn parse_status_scope_args(
    args: &[String],
    scope: StatusScope,
    usage: &str,
) -> Option<(StatusScope, bool)> {
    match args.get(1).map(|arg| arg.as_str()) {
        None => Some((scope, false)),
        Some("--json") if args.len() == 2 => Some((scope, true)),
        _ => {
            eprintln!("usage: {usage}");
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerRuntimeStatus {
    Running {
        version: Option<String>,
        protocol: Option<u32>,
        capabilities: Option<crate::api::schema::ServerCapabilities>,
        identity: crate::api::schema::ServerIdentity,
    },
    NotRunning,
}

fn print_full_status(json: bool) -> std::io::Result<i32> {
    let server = read_server_runtime_status()?;

    if json {
        print_json(&FullStatusJson {
            client: client_status_json(),
            server: server_status_json(&server),
            update: update_status_json(&server),
        })?;
        return Ok(0);
    }

    println!("client:");
    println!("  version: {}", crate::build_info::version());
    println!(
        "  channel: {}",
        crate::config::Config::load().config.update.channel.as_str()
    );
    println!("  protocol: {}", crate::protocol::PROTOCOL_VERSION);
    println!(
        "  endpoint_protocol_generation: {}",
        crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION
    );
    println!("  fork: {}", option_label(crate::build_info::fork_tag()));
    println!();
    println!("server:");
    print_server_status_body(&server, "  ");
    println!();
    println!("update:");
    println!("  restart_needed: {}", restart_needed_label(&server));
    println!(
        "  server_binary_stale: {}",
        server_binary_stale_label(&server)
    );

    Ok(0)
}

fn print_server_status(json: bool) -> std::io::Result<i32> {
    let server = read_server_runtime_status()?;
    if json {
        print_json(&server_status_json(&server))?;
        return Ok(0);
    }
    print_server_status_body(&server, "");
    Ok(0)
}

fn print_client_status(json: bool) -> std::io::Result<()> {
    if json {
        print_json(&client_status_json())?;
        return Ok(());
    }

    println!("version: {}", crate::build_info::version());
    println!(
        "channel: {}",
        crate::config::Config::load().config.update.channel.as_str()
    );
    println!("protocol: {}", crate::protocol::PROTOCOL_VERSION);
    println!(
        "endpoint_protocol_generation: {}",
        crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION
    );
    println!("binary: {}", current_exe_label());
    println!("fork: {}", option_label(crate::build_info::fork_tag()));
    Ok(())
}

fn print_server_status_body(server: &ServerRuntimeStatus, indent: &str) {
    for line in server_status_lines(server, indent) {
        println!("{line}");
    }
}

fn server_status_lines(server: &ServerRuntimeStatus, indent: &str) -> Vec<String> {
    match server {
        ServerRuntimeStatus::Running {
            version,
            protocol,
            capabilities,
            identity,
        } => vec![
            format!("{indent}status: running"),
            format!("{indent}version: {}", option_label(version.as_deref())),
            format!(
                "{indent}binary: {}",
                option_label(identity.binary.as_deref())
            ),
            format!(
                "{indent}fork: {}",
                option_label(identity.fork_tag.as_deref())
            ),
            format!(
                "{indent}commit: {}",
                option_label(identity.commit.as_deref())
            ),
            format!(
                "{indent}sha256: {}",
                option_label(identity.binary_sha256.as_deref())
            ),
            format!(
                "{indent}endpoint_compatible: {}",
                endpoint_compatibility_label(capabilities.as_ref())
            ),
            format!("{indent}private_protocol: {}", protocol_label(*protocol)),
            format!(
                "{indent}private_protocol_compatible: {}",
                compatibility_label(*protocol)
            ),
            format!("{indent}socket: {}", super::target::socket_label()),
        ],
        ServerRuntimeStatus::NotRunning => vec![
            format!("{indent}status: not running"),
            format!("{indent}socket: {}", super::target::socket_label()),
        ],
    }
}

fn read_server_runtime_status() -> std::io::Result<ServerRuntimeStatus> {
    match super::target::server_status(&super::target::api_client()?) {
        Ok(status) => Ok(ServerRuntimeStatus::Running {
            version: status.version,
            protocol: status.protocol,
            capabilities: status.capabilities,
            identity: status.identity,
        }),
        Err(err) if super::target::is_remote() => Err(super::target::remote_error(
            super::api_client_error_to_io(err),
        )),
        Err(ApiClientError::Io(err)) if super::server_not_running_error(&err) => {
            Ok(ServerRuntimeStatus::NotRunning)
        }
        Err(err) => Err(super::api_client_error_to_io(err)),
    }
}

fn option_label(value: Option<&str>) -> &str {
    value.unwrap_or("unknown")
}

fn protocol_label(protocol: Option<u32>) -> String {
    protocol
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn compatibility_label(protocol: Option<u32>) -> &'static str {
    match protocol {
        Some(protocol) if protocol == crate::protocol::PROTOCOL_VERSION => "yes",
        Some(_) => "no",
        None => "unknown",
    }
}

fn endpoint_compatibility_label(
    capabilities: Option<&crate::api::schema::ServerCapabilities>,
) -> &'static str {
    match capabilities.and_then(|value| value.endpoint_protocol_generation) {
        Some(generation)
            if generation == crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION =>
        {
            "yes"
        }
        Some(_) => "no",
        None => "unknown",
    }
}

fn restart_needed_label(server: &ServerRuntimeStatus) -> &'static str {
    match restart_needed_bool(server) {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

fn server_binary_stale_label(server: &ServerRuntimeStatus) -> &'static str {
    match server_binary_stale_bool(server) {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

#[derive(Serialize)]
struct FullStatusJson {
    client: ClientStatusJson,
    server: ServerStatusJson,
    update: UpdateStatusJson,
}

#[derive(Serialize)]
struct ClientStatusJson {
    version: String,
    fork_tag: Option<&'static str>,
    channel: &'static str,
    protocol: u32,
    endpoint_protocol_generation: u32,
    endpoint_capabilities: Vec<&'static str>,
    remote_host_bridge: bool,
    remote_bridge_idle_timeout: bool,
    binary: String,
    session: Option<String>,
}

#[derive(Serialize)]
struct ServerStatusJson {
    status: &'static str,
    running: bool,
    version: Option<String>,
    /// The server's own executable path (the client's is `client.binary`).
    binary: Option<String>,
    fork_tag: Option<String>,
    commit: Option<String>,
    binary_sha256: Option<String>,
    protocol: Option<u32>,
    capabilities: Option<ServerCapabilitiesJson>,
    compatible: Option<bool>,
    endpoint_compatible: Option<bool>,
    socket: String,
    session: Option<String>,
    restart_needed: Option<bool>,
    server_binary_stale: Option<bool>,
}

#[derive(Serialize)]
struct ServerCapabilitiesJson {
    live_handoff: bool,
    detached_server_daemon: bool,
    endpoint_protocol_generation: Option<u32>,
    surface_interest: bool,
    health_check: bool,
    ssh_agent_registration: bool,
}

#[derive(Serialize)]
struct UpdateStatusJson {
    restart_needed: Option<bool>,
    server_binary_stale: Option<bool>,
}

fn client_status_json() -> ClientStatusJson {
    ClientStatusJson {
        version: crate::build_info::version(),
        fork_tag: crate::build_info::fork_tag(),
        channel: crate::config::Config::load().config.update.channel.as_str(),
        protocol: crate::protocol::PROTOCOL_VERSION,
        endpoint_protocol_generation: crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION,
        endpoint_capabilities: vec![
            crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY,
            crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY,
            crate::protocol::endpoint::HEALTH_CHECK_CAPABILITY,
        ],
        remote_host_bridge: true,
        remote_bridge_idle_timeout: crate::platform::REMOTE_BRIDGE_IDLE_TIMEOUT_SUPPORTED,
        binary: current_exe_label(),
        session: crate::session::active_name(),
    }
}

fn server_status_json(server: &ServerRuntimeStatus) -> ServerStatusJson {
    let mut status = match server {
        ServerRuntimeStatus::Running {
            version,
            protocol,
            capabilities,
            identity,
        } => ServerStatusJson {
            status: "running",
            running: true,
            version: version.clone(),
            binary: identity.binary.clone(),
            fork_tag: identity.fork_tag.clone(),
            commit: identity.commit.clone(),
            binary_sha256: identity.binary_sha256.clone(),
            protocol: *protocol,
            capabilities: capabilities
                .as_ref()
                .map(|capabilities| ServerCapabilitiesJson {
                    live_handoff: capabilities.live_handoff,
                    detached_server_daemon: capabilities.detached_server_daemon,
                    endpoint_protocol_generation: capabilities.endpoint_protocol_generation,
                    surface_interest: capabilities.surface_interest,
                    health_check: capabilities.health_check,
                    ssh_agent_registration: capabilities.ssh_agent_registration,
                }),
            compatible: protocol.map(|value| value == crate::protocol::PROTOCOL_VERSION),
            endpoint_compatible: capabilities.as_ref().and_then(|capabilities| {
                capabilities.endpoint_protocol_generation.map(|generation| {
                    generation == crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION
                })
            }),
            socket: api::socket_path().display().to_string(),
            session: crate::session::active_name(),
            restart_needed: restart_needed_bool(server),
            server_binary_stale: server_binary_stale_bool(server),
        },
        ServerRuntimeStatus::NotRunning => ServerStatusJson {
            status: "not_running",
            running: false,
            version: None,
            binary: None,
            fork_tag: None,
            commit: None,
            binary_sha256: None,
            protocol: None,
            capabilities: None,
            compatible: None,
            endpoint_compatible: None,
            socket: api::socket_path().display().to_string(),
            session: crate::session::active_name(),
            restart_needed: Some(false),
            server_binary_stale: Some(false),
        },
    };
    if let Some((_, session)) = super::target::remote_identity() {
        status.socket = super::target::socket_label();
        status.session = Some(session);
        status.server_binary_stale = None;
    }
    status
}

fn update_status_json(server: &ServerRuntimeStatus) -> UpdateStatusJson {
    UpdateStatusJson {
        restart_needed: restart_needed_bool(server),
        server_binary_stale: server_binary_stale_bool(server),
    }
}

fn restart_needed_bool(server: &ServerRuntimeStatus) -> Option<bool> {
    match server {
        ServerRuntimeStatus::Running { capabilities, .. } => Some(
            capabilities
                .as_ref()
                .and_then(|value| value.endpoint_protocol_generation)
                != Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION),
        ),
        ServerRuntimeStatus::NotRunning => Some(false),
    }
}

fn server_binary_stale_bool(server: &ServerRuntimeStatus) -> Option<bool> {
    match server {
        ServerRuntimeStatus::Running {
            version, identity, ..
        } => version.as_deref().map(|version| {
            server_differs_from_client(
                version,
                identity.fork_tag.as_deref(),
                &crate::build_info::version(),
                crate::build_info::fork_tag(),
            )
        }),
        ServerRuntimeStatus::NotRunning => Some(false),
    }
}

/// A server is a different build from this client when the version string differs, or when the
/// fork tags differ: a fork client against another tag, or a tagged client against an untagged
/// (upstream or older fork) server, whose `ping` result carries no tag.
fn server_differs_from_client(
    server_version: &str,
    server_tag: Option<&str>,
    client_version: &str,
    client_tag: Option<&str>,
) -> bool {
    server_version != client_version || server_tag != client_tag
}

fn print_json(value: &impl Serialize) -> std::io::Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn current_exe_label() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("unknown ({err})"))
}

fn print_status_help() {
    eprintln!("herdr status commands:");
    eprintln!("  herdr status [--json]         show local client and running server status");
    eprintln!("  herdr status server [--json]  show running server status");
    eprintln!("  herdr status client [--json]  show local client binary status");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_server(
        version: Option<&str>,
        endpoint_generation: Option<u32>,
    ) -> ServerRuntimeStatus {
        ServerRuntimeStatus::Running {
            version: version.map(str::to_owned),
            protocol: Some(crate::protocol::PROTOCOL_VERSION),
            capabilities: Some(crate::api::schema::ServerCapabilities {
                live_handoff: true,
                detached_server_daemon: true,
                endpoint_protocol_generation: endpoint_generation,
                surface_interest: true,
                health_check: true,
                ssh_agent_registration: false,
            }),
            identity: crate::api::schema::ServerIdentity::default(),
        }
    }

    #[test]
    fn status_exposes_ssh_agent_registration() {
        let server = running_server(Some("test"), None);
        let value = serde_json::to_value(server_status_json(&server)).unwrap();
        assert_eq!(value["capabilities"]["ssh_agent_registration"], false);
    }

    #[test]
    fn stale_compatible_server_does_not_require_restart() {
        let server = running_server(
            Some("0.0.0-old"),
            Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION),
        );

        assert_eq!(restart_needed_bool(&server), Some(false));
        assert_eq!(server_binary_stale_bool(&server), Some(true));
    }

    #[test]
    fn server_without_endpoint_baseline_requires_restart() {
        let server = running_server(Some(crate::build_info::version().as_str()), None);

        assert_eq!(restart_needed_bool(&server), Some(true));
        assert_eq!(server_binary_stale_bool(&server), Some(false));
    }

    fn identity() -> crate::api::schema::ServerIdentity {
        crate::api::schema::ServerIdentity {
            binary: Some("/opt/herdr-fork/bin/herdr".into()),
            fork_tag: Some("v0.9.5-ev.3".into()),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            binary_sha256: Some("ab".repeat(32)),
        }
    }

    #[test]
    fn status_json_carries_the_server_identity() {
        let mut server = running_server(Some("0.9.5"), None);
        let ServerRuntimeStatus::Running { identity: slot, .. } = &mut server else {
            unreachable!()
        };
        *slot = identity();
        let value = serde_json::to_value(server_status_json(&server)).unwrap();
        assert_eq!(value["binary"], "/opt/herdr-fork/bin/herdr");
        assert_eq!(value["fork_tag"], "v0.9.5-ev.3");
        assert_eq!(value["commit"], "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(value["binary_sha256"], "ab".repeat(32));
    }

    #[test]
    fn client_status_json_carries_the_fork_tag() {
        let value = serde_json::to_value(client_status_json()).unwrap();
        assert_eq!(value["fork_tag"].as_str(), crate::build_info::fork_tag());
        assert!(value.get("fork_tag").is_some());
    }

    #[test]
    fn status_json_reports_null_for_an_untagged_unhashed_server() {
        let server = running_server(Some("0.9.5"), None);
        let value = serde_json::to_value(server_status_json(&server)).unwrap();
        for key in ["binary", "fork_tag", "commit", "binary_sha256"] {
            assert!(value.get(key).is_some_and(|v| v.is_null()), "{key}");
        }
    }

    #[test]
    fn human_status_shows_binary_fork_commit_and_sha256() {
        let mut server = running_server(Some("0.9.5"), None);
        let ServerRuntimeStatus::Running { identity: slot, .. } = &mut server else {
            unreachable!()
        };
        *slot = identity();
        let lines = server_status_lines(&server, "");
        for want in [
            "binary: /opt/herdr-fork/bin/herdr".to_string(),
            "fork: v0.9.5-ev.3".to_string(),
            "commit: 0123456789abcdef0123456789abcdef01234567".to_string(),
            format!("sha256: {}", "ab".repeat(32)),
        ] {
            assert!(lines.contains(&want), "{want} in {lines:?}");
        }
    }

    #[test]
    fn human_status_says_unknown_for_an_older_server() {
        let lines = server_status_lines(&running_server(Some("0.9.5"), None), "  ");
        assert!(lines.contains(&"  fork: unknown".to_string()), "{lines:?}");
        assert!(
            lines.contains(&"  sha256: unknown".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn the_same_version_and_tag_is_not_a_mismatch() {
        assert!(!server_differs_from_client(
            "0.9.5",
            Some("v0.9.5-ev.3"),
            "0.9.5",
            Some("v0.9.5-ev.3")
        ));
        assert!(!server_differs_from_client("0.9.5", None, "0.9.5", None));
    }

    #[test]
    fn a_tagged_client_against_an_untagged_server_is_a_mismatch() {
        assert!(server_differs_from_client(
            "0.9.5",
            None,
            "0.9.5",
            Some("v0.9.5-ev.3")
        ));
    }

    #[test]
    fn an_untagged_client_against_a_tagged_server_is_a_mismatch() {
        assert!(server_differs_from_client(
            "0.9.5",
            Some("v0.9.5-ev.3"),
            "0.9.5",
            None
        ));
    }

    #[test]
    fn differing_fork_tags_are_a_mismatch() {
        assert!(server_differs_from_client(
            "0.9.5",
            Some("v0.9.5-ev.2"),
            "0.9.5",
            Some("v0.9.5-ev.3")
        ));
    }

    #[test]
    fn a_server_with_a_different_version_is_still_a_mismatch() {
        assert!(server_differs_from_client("0.9.4", None, "0.9.5", None));
    }

    #[test]
    fn a_server_running_another_fork_tag_is_reported_stale() {
        let mut server = running_server(Some(crate::build_info::version().as_str()), None);
        let ServerRuntimeStatus::Running { identity: slot, .. } = &mut server else {
            unreachable!()
        };
        slot.fork_tag = Some("v0.0.0-other".into());
        assert_eq!(server_binary_stale_bool(&server), Some(true));
    }
}
