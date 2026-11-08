/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! The custody daemon (docs/architecture/key-custody.md): a native sidecar in every pod, started before `polar-init`,
//! holding the pod's mTLS keys and signing for the workload over a socket.
//!
//! Its environment, every problem named together:
//! - `POLAR_CUSTODY_SOCKET`: where the socket is, on a volume only the pod's containers share;
//! - `POLAR_CUSTODY_CA_CERT`: the CA the pod was given, which every certificate installed must come from;
//! - `POLAR_CUSTODY_INSTALLER_UID`: `polar-init`'s UID;
//! - `POLAR_CUSTODY_WORKLOAD_UIDS`: the workload containers' UIDs, comma-separated;
//! - `BROKER_ADDR` and `CASSINI_SERVER_NAME`: Cassini, where it watches as its pod for its own revocation (step 5);
//! - `POLAR_AUTHORITY_PUBLISHER`: the credentials agent's identity, by default its service account, as every agent's.
//!
//! When the credentials agent revokes a certificate it holds, it destroys every key and exits with [`REVOKED`].

use polar_custody::custody::Custody;
use polar_custody::server::{Callers, bind, serve};
use polar_custody_daemon::watch::{DEFAULT_PUBLISHER, PUBLISHER_VARIABLE, Watcher, WatcherArgs};
use std::sync::{Arc, RwLock};
use tracing::{error, info, warn};

/// The exit code once a certificate it held is revoked and its keys are destroyed.
const REVOKED: i32 = 3;
const CUSTODIAN_FAILED: i32 = 4;

struct Settings {
    socket: String,
    ca_pem: String,
    callers: Callers,
    broker: String,
    server_name: String,
    publisher: String,
}

fn settings() -> Result<Settings, Vec<String>> {
    let mut problems = Vec::new();
    let mut var = |name: &str| match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        _ => {
            problems.push(format!("{name} is required"));
            None
        }
    };
    let socket = var("POLAR_CUSTODY_SOCKET");
    let ca_path = var("POLAR_CUSTODY_CA_CERT");
    let installer = var("POLAR_CUSTODY_INSTALLER_UID");
    let workload = var("POLAR_CUSTODY_WORKLOAD_UIDS");
    let broker = var("BROKER_ADDR");
    let server_name = var("CASSINI_SERVER_NAME");
    let publisher = std::env::var(PUBLISHER_VARIABLE)
        .ok()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| DEFAULT_PUBLISHER.to_string());
    let ca_pem = ca_path
        .as_ref()
        .and_then(|path| match std::fs::read_to_string(path) {
            Ok(pem) => Some(pem),
            Err(e) => {
                problems.push(format!("POLAR_CUSTODY_CA_CERT: cannot read {path}: {e}"));
                None
            }
        });
    let installer_uid = installer.and_then(|uid| match uid.parse::<u32>() {
        Ok(uid) => Some(uid),
        Err(_) => {
            problems.push(format!("POLAR_CUSTODY_INSTALLER_UID: `{uid}` is not a UID"));
            None
        }
    });
    let workload_uids = workload.and_then(|uids| {
        match uids
            .split(',')
            .map(|u| u.trim().parse::<u32>())
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(uids) => Some(uids),
            Err(_) => {
                problems.push(format!(
                    "POLAR_CUSTODY_WORKLOAD_UIDS: `{uids}` is not a list of UIDs"
                ));
                None
            }
        }
    });
    match (
        socket,
        ca_pem,
        installer_uid,
        workload_uids,
        broker,
        server_name,
    ) {
        (
            Some(socket),
            Some(ca_pem),
            Some(installer_uid),
            Some(workload_uids),
            Some(broker),
            Some(server_name),
        ) if problems.is_empty() => Ok(Settings {
            socket,
            ca_pem,
            callers: Callers {
                installer_uid,
                workload_uids,
            },
            broker,
            server_name,
            publisher,
        }),
        _ => Err(problems),
    }
}

/// Not dumpable, so no core file and no ptrace by its own UID; its memory locked, so no key reaches swap. Each is
/// logged where it cannot be had: the daemon still runs, with less.
fn harden() {
    // SAFETY: prctl takes no pointers here; it changes only this process.
    let dumpable = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    if dumpable != 0 {
        warn!(
            "cannot make the process undumpable: {}",
            std::io::Error::last_os_error()
        );
    }
    if let Err(e) = polar_custody_daemon::lock_memory() {
        warn!("cannot lock the process's memory: {e}");
    }
}

/// What it logs: `RUST_LOG`, or `info` when that is unset or does not parse, as every agent (`polar::logging`).
fn console_filter(rust_log: Option<String>) -> tracing_subscriber::EnvFilter {
    rust_log
        .and_then(|filter| tracing_subscriber::EnvFilter::try_new(filter).ok())
        .unwrap_or_else(|| tracing_subscriber::EnvFilter::new("info"))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(console_filter(std::env::var("RUST_LOG").ok()))
        .init();
    let settings = match settings() {
        Ok(settings) => settings,
        Err(problems) => {
            for problem in &problems {
                error!("{problem}");
            }
            std::process::exit(2);
        }
    };
    harden();
    let custody = match Custody::new(&settings.ca_pem) {
        Ok(custody) => Arc::new(RwLock::new(custody)),
        Err(e) => {
            error!("the pinned CA: {e}");
            std::process::exit(2);
        }
    };
    let listener = match bind(std::path::Path::new(&settings.socket)) {
        Ok(listener) => listener,
        Err(e) => {
            error!("cannot listen on {}: {e}", settings.socket);
            std::process::exit(1);
        }
    };
    let watching = WatcherArgs {
        custody: custody.clone(),
        ca_pem: settings.ca_pem.clone(),
        broker: settings.broker.clone(),
        server_name: settings.server_name.clone(),
        publisher: settings.publisher.clone(),
        // Its keys are destroyed already (`Custody::revoke`): the kubelet restarts it empty.
        on_revoked: Arc::new(|slots| {
            error!(?slots, "revoked: exiting");
            std::process::exit(REVOKED);
        }),
    };
    if let Err(e) = ractor::Actor::spawn(None, Watcher, watching).await {
        error!("cannot watch for this pod's revocation: {e}");
        std::process::exit(1);
    }
    // A custodian that failed holds what is spent: the kubelet starts the daemon again empty, for polar-init's next run
    // in this pod (docs/architecture/key-custody.md, "A retry in place").
    tokio::spawn(polar_custody_daemon::end_once_failed(
        custody.clone(),
        polar_custody_daemon::FAILURE_LOOK_EVERY,
        polar_custody_daemon::FAILED_GRACE,
        |why| {
            error!(%why, "the custodian could not fetch what it was asked to: exiting, to start again empty");
            std::process::exit(CUSTODIAN_FAILED);
        },
    ));
    info!(socket = %settings.socket, installer = settings.callers.installer_uid, workload = ?settings.callers.workload_uids, broker = %settings.broker, publisher = %settings.publisher, "custody daemon serving");
    if let Err(e) = serve(listener, custody, Arc::new(settings.callers)).await {
        error!("the socket failed: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_logs_at_info_unless_rust_log_says_otherwise_as_every_agent_does() {
        assert_eq!(console_filter(None).to_string(), "info");
        assert_eq!(console_filter(Some("debug".into())).to_string(), "debug");
        assert_eq!(
            console_filter(Some("not a filter [".into())).to_string(),
            "info",
            "a filter that does not parse would log nothing it was meant to"
        );
    }
}

// Every Polar binary allocates with Scudo (docs/architecture/broker-baseline.md, "Decided").
#[global_allocator]
static GLOBAL: polar_scudo::Scudo = polar_scudo::Scudo;
