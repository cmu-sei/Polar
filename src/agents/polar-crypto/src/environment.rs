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

//! The environment the crypto actor reads, each variable declared once.
//!
//! The actor derives its own identity from the client certificate the pod was issued, believes a
//! recipient's key under the pod's pinned CA, and asks the pod's custody daemon to unwrap what is
//! sealed to its escrow key. All three are the Cassini client's variables (`TLS_CLIENT_CERT`,
//! `TLS_CA_CERT`, `POLAR_CUSTODY_SOCKET`), declared once by `cassini-client`; this crate declares
//! nothing of its own since the workload stopped fetching its escrow key itself
//! (docs/architecture/key-custody.md, step 9).

use cassini_client::environment::{CUSTODY_SOCKET, TLS_CA_CERT, TLS_CLIENT_CERT};
use polar_env::{Described, Env, Section};

static VARIABLES: &[&dyn Described] = &[];

/// What the crypto actor reads beyond the Cassini client's own variables. Applies to every
/// agent with the encryption capability, which always has the Cassini client too.
pub static SECTION: Section = Section {
    name: "crypto",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

/// Everything the crypto actor is configured with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CryptoSettings {
    /// The certificate this agent was issued, whose identity the actor takes as its own.
    pub own_certificate_path: String,
    /// The custody daemon's socket, when the pod has one: it holds the escrow key, and unwraps for this actor
    /// (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian").
    pub custody_socket: Option<String>,
    /// The CA the pod pins, which a recipient's escrow certificate must chain to.
    pub ca_cert_path: String,
}

impl CryptoSettings {
    /// Reads from `env`, naming every variable that is missing or malformed.
    pub fn from_env(env: &dyn Env) -> Result<Self, String> {
        let mut problems = Vec::new();
        let mut take = |read: Result<String, polar_env::EnvError>| match read {
            Ok(value) => value,
            Err(error) => {
                problems.push(error.to_string());
                String::new()
            }
        };
        let own_certificate_path = take(TLS_CLIENT_CERT.read(env));
        let ca_cert_path = take(TLS_CA_CERT.read(env));
        let custody_socket = match CUSTODY_SOCKET.read(env) {
            Ok(socket) => socket.filter(|s| !s.trim().is_empty()),
            Err(error) => {
                problems.push(error.to_string());
                None
            }
        };
        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        Ok(Self {
            own_certificate_path,
            custody_socket,
            ca_cert_path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_custody_daemon_s_socket_and_the_pinned_ca_are_the_client_s_own_variables() {
        let s = CryptoSettings::from_env(&env(&[
            ("TLS_CLIENT_CERT", "/c/cert.pem"),
            ("TLS_CA_CERT", "/c/ca.pem"),
            ("POLAR_CUSTODY_SOCKET", "/run/custody/custody.sock"),
        ]))
        .unwrap();
        assert_eq!(
            s.custody_socket.as_deref(),
            Some("/run/custody/custody.sock")
        );
        assert_eq!(s.ca_cert_path, "/c/ca.pem");
        let s = CryptoSettings::from_env(&env(&[
            ("TLS_CLIENT_CERT", "/c/cert.pem"),
            ("TLS_CA_CERT", "/c/ca.pem"),
        ]))
        .unwrap();
        assert_eq!(s.custody_socket, None, "no daemon: no escrow key");
        let error =
            CryptoSettings::from_env(&env(&[("TLS_CLIENT_CERT", "/c/cert.pem")])).unwrap_err();
        assert!(error.contains("TLS_CA_CERT"), "{error}");
    }

    #[test]
    fn a_missing_certificate_path_is_named_with_what_it_is_for() {
        let error = CryptoSettings::from_env(&env(&[])).unwrap_err();
        assert!(error.contains("TLS_CLIENT_CERT"), "{error}");
        assert!(error.contains("certificate"), "{error}");
    }

    /// The token and the kernel key's handle went with the workload's own escrow bootstrap: an old pod's
    /// `KUBERNETES_SA_TOKEN` or `POLAR_ESCROW_KEY_HANDLE` is not declared, so it is not even read.
    #[test]
    fn the_section_declares_nothing_of_its_own() {
        let names: Vec<_> = SECTION.describe().iter().map(|v| v.name).collect();
        assert!(
            names.is_empty(),
            "TLS_CLIENT_CERT is the client's: {names:?}"
        );
        assert!(
            SECTION.check(&env(&[])).is_empty(),
            "nothing in it is required"
        );
    }
}
