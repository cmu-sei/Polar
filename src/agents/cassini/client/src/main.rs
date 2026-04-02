use anyhow::Result;
use cassini_client::TCPClientConfig;
use cassini_client::TcpClientMessage;
use cassini_client::cli::*;
use cassini_tracing::shutdown_tracing;
use cassini_types::ControlOp;
use clap::Parser;
use std::path::PathBuf;
use tokio::time::Duration;
// ===== Helpers =====

fn resolve_socket_path(override_path: Option<&PathBuf>) -> PathBuf {
    if let Some(p) = override_path {
        return p.clone();
    }
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime).join("cassini/daemon.sock")
    } else {
        PathBuf::from(DEFAULT_SOCK_PATH)
    }
}

// ===== Entry point =====
//

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let register_timeout = Duration::from_secs(cli.register_timeout);
    let format = &cli.format;
    let socket_path = resolve_socket_path(cli.socket.as_ref());

    // Load TLS + broker config from env
    // TODO: We default to this approach, but perhaps eventually we want to provide values via CLI/config file?
    let client_config = TCPClientConfig::new()
        .map_err(|e| anyhow::anyhow!("Failed to load config from environment: {e}"))?;

    // Daemon start path — no subcommand needed.
    if cli.daemon {
        //only init_tracing and logs in daemon mode!!
        cassini_tracing::init_tracing("cassini-cli");

        let result = run_daemon(
            socket_path,
            client_config.clone(),
            register_timeout,
            cli.foreground,
        )
        .await;
        if let Err(e) = &result {
            eprintln!("Error: {e}");
            cassini_tracing::shutdown_tracing();
            std::process::exit(1);
        }
        cassini_tracing::shutdown_tracing();
        return result;
    }

    let command = match cli.command {
        Some(c) => c,
        None => {
            eprintln!("No command given. Try `cassini-client --help`.");
            cassini_tracing::shutdown_tracing();
            std::process::exit(1);
        }
    };

    // Determine transport: daemon socket if reachable, direct connect otherwise.
    // If the user explicitly set a socket path (via --socket or env var) and it
    // is not reachable, fail loudly rather than silently falling back (Option B).
    let use_daemon = daemon_is_reachable(&socket_path).await;
    let socket_explicitly_set =
        cli.socket.is_some() || std::env::var("CASSINI_DAEMON_SOCK").is_ok();

    if socket_explicitly_set && !use_daemon {
        anyhow::bail!(
            "Daemon socket at {} was explicitly configured but is not reachable.\n\
             Start the daemon with `cassini --daemon` or unset CASSINI_DAEMON_SOCK.",
            socket_path.display()
        );
    }

    let result = if use_daemon {
        // Route through the daemon socket.
        match command {
            Command::Publish {
                topic,
                payload,
                publish_timeout,
            } => {
                let payload_b64 = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    payload.as_bytes(),
                );
                dispatch_to_daemon(
                    &socket_path,
                    IpcRequest::Publish {
                        topic,
                        payload_b64,
                        timeout_secs: publish_timeout,
                    },
                    format,
                )
                .await
            }
            Command::ListSessions { timeout } => {
                dispatch_to_daemon(
                    &socket_path,
                    IpcRequest::ListSessions {
                        timeout_secs: timeout,
                    },
                    format,
                )
                .await
            }
            Command::ListTopics { timeout } => {
                dispatch_to_daemon(
                    &socket_path,
                    IpcRequest::ListTopics {
                        timeout_secs: timeout,
                    },
                    format,
                )
                .await
            }
            Command::GetSession {
                registration_id,
                timeout,
            } => {
                dispatch_to_daemon(
                    &socket_path,
                    IpcRequest::GetSession {
                        registration_id,
                        timeout_secs: timeout,
                    },
                    format,
                )
                .await
            }
            Command::Status => dispatch_to_daemon(&socket_path, IpcRequest::Status, format).await,
        }
    } else {
        // Direct connect — one registration per invocation.
        match command {
            Command::Publish {
                topic,
                payload,
                publish_timeout,
            } => {
                run_publish_direct(
                    client_config.clone(),
                    topic,
                    payload,
                    register_timeout,
                    Duration::from_secs(publish_timeout),
                    format,
                )
                .await
            }
            Command::ListSessions { timeout } => {
                run_control_direct(
                    client_config.clone(),
                    TcpClientMessage::ListSessions { trace_ctx: None },
                    register_timeout,
                    Duration::from_secs(timeout),
                    "Timed out waiting for session list",
                    format,
                )
                .await
            }
            Command::ListTopics { timeout } => {
                run_control_direct(
                    client_config.clone(),
                    TcpClientMessage::ListTopics { trace_ctx: None },
                    register_timeout,
                    Duration::from_secs(timeout),
                    "Timed out waiting for topic list",
                    format,
                )
                .await
            }
            Command::GetSession {
                registration_id,
                timeout,
            } => {
                run_control_direct(
                    client_config.clone(),
                    TcpClientMessage::ControlRequest {
                        op: ControlOp::GetSessionInfo { registration_id },
                        trace_ctx: None,
                    },
                    register_timeout,
                    Duration::from_secs(timeout),
                    "Timed out waiting for session details",
                    format,
                )
                .await
            }
            Command::Status => {
                eprintln!("No daemon running at {}", socket_path.display());
                std::process::exit(1);
            }
        }
    };

    if let Err(e) = &result {
        eprintln!("Error: {e}");
        shutdown_tracing();
        std::process::exit(1);
    }

    shutdown_tracing();
    result
}
