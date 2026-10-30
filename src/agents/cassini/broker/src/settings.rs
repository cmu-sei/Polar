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

//! What the broker reads from its configuration, declared once.
//!
//! These were read with `std::env::var` in `BrokerArgs::new`, so nothing could list them, check
//! them before the pod started, or say what they were for. Declared here they are in the broker's
//! scaffold and judged with everything else (the builder's, the health actor's, how it logs). The broker is
//! not a Cassini client, so none of the client's variables are asked of it.

use polar_custody::client::ServerKey;
use polar_custody::environment::{CUSTODY_SOCKET, KeyGiven, one_key};
use polar_env::{Config, ConfigError, Defaulted, Described, Env, Optional, Required, Section};

pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";

pub static SERVER_CERT_CHAIN: Required<String> = Required::text(
    "TLS_SERVER_CERT_CHAIN",
    "path to the broker's own certificate (PEM), which it presents to every client",
);

const SERVER_KEY_DOC: &str = "path to the private key for the broker's certificate; this or POLAR_CUSTODY_SOCKET, the custody daemon holding it, not both";

pub static SERVER_KEY: Optional<String> = Optional::text("TLS_SERVER_KEY", SERVER_KEY_DOC);

pub static CA_CERT: Required<String> = Required::text(
    "TLS_CA_CERT",
    "path to the CA certificate the clients' certificates are verified against",
);

pub static BIND_ADDR: Defaulted<String> = Defaulted::text(
    "CASSINI_BIND_ADDR",
    "the address the broker listens on, host:port",
    DEFAULT_BIND_ADDR,
);

pub static SHUTDOWN_TOKEN: Required<String> = Required::text(
    "CASSINI_SHUTDOWN_TOKEN",
    "the token a client must present to ask the broker to shut down over the wire; the broker's \
     own orderly shutdown (a signal, a health verdict) presents it itself",
)
.secret();

/// How long a connection keeps the broker's decision about a topic (docs/architecture/policy-enforcement.md,
/// "Decisions"). A change of authority makes every kept decision stale at once; the time bounds how late a rule that
/// reads the clock takes effect. Zero keeps none, which is how enforcement without the cache is measured.
pub static DECISION_CACHE_SECS: Defaulted<u64> = Defaulted::seconds(
    "CASSINI_DECISION_CACHE_SECS",
    "how long a connection keeps the broker's decision about a topic, in seconds; a change of policy or groups ends it \
     at once, and 0 keeps none",
    "5",
);

/// The broker's memory budget (docs/architecture/cassini-backpressure.md, "Decided" 2): every payload byte it holds is
/// counted against it, and when it is exhausted the broker stops reading until it has drained. Unset, it is half the
/// container's memory limit, or 256 MiB where there is none (`crate::budget::default_budget`).
pub static MEMORY_BUDGET_BYTES: Optional<u64> = Optional::number(
    "CASSINI_MEMORY_BUDGET_BYTES",
    "the payload bytes the broker holds at most, in bytes; unset, half the container's memory limit (cgroup v2 \
     memory.max), or 256 MiB where there is none; at least one frame, 67108864, or the broker does not start",
);

/// How long a subscriber may hold its topics back above its high water before its connection is ended
/// (docs/architecture/cassini-backpressure.md, "Decided" 1).
pub static STALL_SECS: Defaulted<u64> = Defaulted::seconds(
    "CASSINI_STALL_SECS",
    "how long a subscriber may hold a topic back, above its high water of unacknowledged deliveries, before the \
     broker ends its connection, in seconds; more than 0 and under 60 (TCP_USER_TIMEOUT)",
    "30",
);

static VARIABLES: &[&dyn Described] = &[
    &SERVER_CERT_CHAIN,
    &SERVER_KEY,
    &CUSTODY_SOCKET,
    &CA_CERT,
    &BIND_ADDR,
    &SHUTDOWN_TOKEN,
    &DECISION_CACHE_SECS,
    &MEMORY_BUDGET_BYTES,
    &STALL_SECS,
];

/// The section the broker registers.
pub static SECTION: Section = Section {
    name: "cassini-broker",
    vars: VARIABLES,
    relations: |env| {
        let mut problems: Vec<String> = read_server_key(env)
            .err()
            .map(|e| e.to_string())
            .into_iter()
            .collect();
        if let Ok(stall) = STALL_SECS.read(env)
            && let Ok(budget) = MEMORY_BUDGET_BYTES.read(env)
            && let Err(why) = crate::budget::Bounds::new(
                budget
                    .map(|b| b as usize)
                    .unwrap_or(crate::budget::FALLBACK_BUDGET),
                std::time::Duration::from_secs(stall),
            )
        {
            problems.push(why);
        }
        problems
    },
};

/// The broker's key: a file, or the custody daemon's server slot (docs/architecture/key-custody.md, step 4). Exactly
/// one is given.
pub fn read_server_key(env: &dyn Env) -> Result<ServerKey, polar_env::EnvError> {
    Ok(match one_key(env, &SERVER_KEY, SERVER_KEY_DOC)? {
        KeyGiven::File(path) => ServerKey::File(path),
        KeyGiven::Socket(socket) => ServerKey::Custody(socket),
    })
}

pub struct BrokerSettings {
    pub server_cert_file: String,
    /// The certificate's key: a file, or the custody daemon's.
    pub server_key: ServerKey,
    pub ca_cert_file: String,
    pub bind_addr: String,
    pub shutdown_auth_token: String,
    pub decision_cache: std::time::Duration,
    /// The memory budget the deployment gives, if it gives one.
    pub memory_budget: Option<u64>,
    pub stall: std::time::Duration,
}

impl BrokerSettings {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            server_cert_file: config.read(&SERVER_CERT_CHAIN)?,
            server_key: read_server_key(config).map_err(ConfigError::Env)?,
            ca_cert_file: config.read(&CA_CERT)?,
            bind_addr: config.read(&BIND_ADDR)?,
            shutdown_auth_token: config.read(&SHUTDOWN_TOKEN)?,
            decision_cache: std::time::Duration::from_secs(config.read(&DECISION_CACHE_SECS)?),
            memory_budget: config.read(&MEMORY_BUDGET_BYTES)?,
            stall: std::time::Duration::from_secs(config.read(&STALL_SECS)?),
        })
    }

    /// The broker's bounds: the budget given, or half the container's memory limit, or the fallback with none; and
    /// the stall bound. Refused, saying why, when they cannot work.
    pub fn bounds(&self) -> Result<crate::budget::Bounds, String> {
        use crate::budget::{Bounds, MEMORY_MAX_PATH, default_budget, read_memory_max};
        let budget = match self.memory_budget {
            Some(given) => given as usize,
            None => default_budget(read_memory_max(MEMORY_MAX_PATH)),
        };
        Bounds::new(budget, self.stall)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> Config {
        let env: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::builder(&env).register(&SECTION).build()
    }

    const MINIMUM: &[(&str, &str)] = &[
        ("TLS_SERVER_CERT_CHAIN", "/tls/cert.pem"),
        ("TLS_SERVER_KEY", "/tls/key.pem"),
        ("TLS_CA_CERT", "/tls/ca.pem"),
        ("CASSINI_SHUTDOWN_TOKEN", "t0ken"),
    ];

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn the_certificate_the_key_the_ca_and_the_token_are_enough() {
        let settings = BrokerSettings::read(&config(MINIMUM)).unwrap();
        assert_eq!(settings.server_cert_file, "/tls/cert.pem");
        assert_eq!(settings.server_key, ServerKey::File("/tls/key.pem".into()));
        assert_eq!(settings.ca_cert_file, "/tls/ca.pem");
        assert_eq!(settings.bind_addr, DEFAULT_BIND_ADDR);
        assert_eq!(settings.shutdown_auth_token, "t0ken");
        assert_eq!(settings.decision_cache, std::time::Duration::from_secs(5));
        assert_eq!(settings.memory_budget, None);
        assert_eq!(settings.stall, std::time::Duration::from_secs(30));
    }

    #[test]
    fn the_budget_and_the_stall_bound_are_read_and_judged() {
        let mut vars = MINIMUM.to_vec();
        vars.extend([
            ("CASSINI_MEMORY_BUDGET_BYTES", "134217728"),
            ("CASSINI_STALL_SECS", "5"),
        ]);
        let settings = BrokerSettings::read(&config(&vars)).unwrap();
        let bounds = settings.bounds().unwrap();
        assert_eq!(bounds.budget.limit(), 134_217_728);
        assert_eq!(bounds.stall, std::time::Duration::from_secs(5));

        for (name, value, why) in [
            ("CASSINI_MEMORY_BUDGET_BYTES", "1048576", "below one frame"),
            ("CASSINI_STALL_SECS", "60", "TCP_USER_TIMEOUT"),
        ] {
            let mut vars = MINIMUM.to_vec();
            vars.push((name, value));
            let problems = config(&vars).check().join(" | ");
            assert!(problems.contains(why), "{name}={value}: {problems}");
            assert!(
                BrokerSettings::read(&config(&vars))
                    .unwrap()
                    .bounds()
                    .is_err()
            );
        }
    }

    #[test]
    fn what_is_set_overrides_the_defaults() {
        let mut vars = MINIMUM.to_vec();
        vars.extend([
            ("CASSINI_BIND_ADDR", "0.0.0.0:9000"),
            ("CASSINI_DECISION_CACHE_SECS", "0"),
        ]);
        let settings = BrokerSettings::read(&config(&vars)).unwrap();
        assert_eq!(settings.bind_addr, "0.0.0.0:9000");
        assert_eq!(settings.decision_cache, std::time::Duration::ZERO);
    }

    #[test]
    fn everything_missing_is_named_with_what_it_is_for() {
        let problems = config(&[]).check().join(" | ");
        for (name, what) in [
            ("TLS_SERVER_CERT_CHAIN", "presents to every client"),
            ("TLS_CA_CERT", "clients' certificates"),
            ("CASSINI_SHUTDOWN_TOKEN", "shut down over the wire"),
        ] {
            assert!(
                problems.contains(name) && problems.contains(what),
                "{name}: {problems}"
            );
        }
        // The key is judged with the custody socket, once the variables it is judged with are given.
        let mut keyless = MINIMUM.to_vec();
        keyless.retain(|(k, _)| *k != "TLS_SERVER_KEY");
        let problems = config(&keyless).check().join(" | ");
        assert!(
            problems.contains("TLS_SERVER_KEY")
                && problems.contains("private key")
                && problems.contains("POLAR_CUSTODY_SOCKET"),
            "{problems}"
        );
    }

    // docs/architecture/key-custody.md, step 4: the broker's key may be the custody daemon's server slot.
    #[test]
    fn the_key_may_be_the_custody_daemon_s_and_is_one_or_the_other() {
        let mut custody = MINIMUM.to_vec();
        custody.retain(|(k, _)| *k != "TLS_SERVER_KEY");
        custody.push(("POLAR_CUSTODY_SOCKET", "/run/polar-custody/custody.sock"));
        assert!(config(&custody).check().is_empty());
        assert_eq!(
            BrokerSettings::read(&config(&custody)).unwrap().server_key,
            ServerKey::Custody("/run/polar-custody/custody.sock".into())
        );
        let mut both = MINIMUM.to_vec();
        both.push(("POLAR_CUSTODY_SOCKET", "/run/polar-custody/custody.sock"));
        let problems = config(&both).check().join(" | ");
        assert!(problems.contains("set one"), "{problems}");
        assert!(BrokerSettings::read(&config(&both)).is_err());
    }

    #[test]
    fn the_token_is_never_shown_in_a_report() {
        let report = config(MINIMUM).report();
        let token = report
            .iter()
            .find(|r| r.name == "CASSINI_SHUTDOWN_TOKEN")
            .unwrap();
        assert_eq!(token.value.as_deref(), Some("<hidden>"));
    }
}
