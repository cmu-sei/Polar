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

/// A cap on the broker's memory budget (docs/architecture/cassini-backpressure.md, "Runtime budget"): the budget is set
/// at run time from what memory allows and how fast subscribers drain (`crate::runtime_budget`), never above this.
pub static MEMORY_BUDGET_BYTES: Optional<u64> = Optional::number(
    "CASSINI_MEMORY_BUDGET_BYTES",
    "a cap on the payload bytes the broker holds, in bytes; the budget is set at run time from the memory the host \
     and the container allow and how fast subscribers drain, never above this; unset, no cap; at least the floor, \
     four frames (268435456), or the broker does not start",
);

/// How long a subscriber may hold its topics back above its high water before its connection is ended
/// (docs/architecture/cassini-backpressure.md, "Decided" 1).
pub static STALL_SECS: Defaulted<u64> = Defaulted::seconds(
    "CASSINI_STALL_SECS",
    "how long a subscriber may hold a topic back, above its high water of unacknowledged deliveries, before the \
     broker ends its connection, in seconds; more than 0 and under 60 (TCP_USER_TIMEOUT)",
    "30",
);

/// The runtime budget's latency target, as a share of the stall bound (docs/architecture/cassini-backpressure.md,
/// "Runtime budget"; `Tunables::latency_share`). Its default is the controller's own, marked for the maintainer's
/// review; a variable so the benchmark can measure others (broker-baseline.md, the fifth set).
pub static BUDGET_LATENCY_PERCENT: Defaulted<u64> = Defaulted::number(
    "CASSINI_BUDGET_LATENCY_PERCENT",
    "the memory budget's latency target, as a percentage of the stall bound (CASSINI_STALL_SECS): the budget grows \
     toward the subscribers' drain rate times this; more than 0",
    "100",
);

/// How much the runtime budget grows, or falls toward its target, in one sample (`Tunables::step`). Its default is the
/// controller's own, marked for the maintainer's review.
pub static BUDGET_STEP_BYTES: Defaulted<u64> = Defaulted::number(
    "CASSINI_BUDGET_STEP_BYTES",
    "how much the memory budget grows, or falls toward its target, in one sample (a second), in bytes; more than 0",
    "16777216",
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
    &BUDGET_LATENCY_PERCENT,
    &BUDGET_STEP_BYTES,
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
            && let Err(why) = crate::budget::Bounds::new(
                crate::runtime_budget::Tunables::default().floor() as usize,
                std::time::Duration::from_secs(stall),
            )
        {
            problems.push(why);
        }
        // A cap under the floor cannot work, whatever memory allows.
        if let Ok(cap) = MEMORY_BUDGET_BYTES.read(env)
            && let Err(why) = crate::runtime_budget::startup_check(
                &crate::runtime_budget::MemorySample::default(),
                &crate::runtime_budget::Tunables::default(),
                cap,
            )
        {
            problems.push(why);
        }
        for (variable, value) in [
            (&BUDGET_LATENCY_PERCENT, BUDGET_LATENCY_PERCENT.read(env)),
            (&BUDGET_STEP_BYTES, BUDGET_STEP_BYTES.read(env)),
        ] {
            if value == Ok(0) {
                problems.push(format!(
                    "{} is 0: the memory budget could never grow; give more than 0",
                    variable.desc().name
                ));
            }
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
    /// The cap the deployment puts on the memory budget, if it puts one.
    pub memory_budget: Option<u64>,
    pub stall: std::time::Duration,
    /// The runtime budget's latency target, in percent of the stall bound.
    pub budget_latency_percent: u64,
    /// The runtime budget's step, in bytes a sample.
    pub budget_step: u64,
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
            budget_latency_percent: config.read(&BUDGET_LATENCY_PERCENT)?,
            budget_step: config.read(&BUDGET_STEP_BYTES)?,
        })
    }

    /// The controller's tunables: its defaults, with the latency target and step as set.
    pub fn tunables(&self) -> crate::runtime_budget::Tunables {
        crate::runtime_budget::Tunables {
            latency_share: lowest_terms(self.budget_latency_percent, 100),
            step: self.budget_step,
            ..crate::runtime_budget::Tunables::default()
        }
    }

    /// The broker's bounds: the budget starts at its floor (there is no demand yet to measure), and the stall bound.
    /// Refused, saying why, when they cannot work.
    pub fn bounds(&self) -> Result<crate::budget::Bounds, String> {
        let tunables = crate::runtime_budget::Tunables::default();
        crate::budget::Bounds::new(tunables.floor() as usize, self.stall)
    }

    /// The runtime budget, reading the host's and the container's memory: refused, saying what it found and what to
    /// give, when memory cannot give the floor at start or the cap is under it.
    pub fn budget_control(&self) -> Result<crate::pressure::BudgetControl, String> {
        use crate::runtime_budget::{Controller, MemorySource, SystemMemory, startup_check};
        let tunables = self.tunables();
        let memory = SystemMemory::default();
        startup_check(&memory.sample(), &tunables, self.memory_budget)?;
        Ok(crate::pressure::BudgetControl {
            controller: Controller::new(tunables, self.stall, self.memory_budget),
            memory: std::sync::Arc::new(memory),
        })
    }
}

/// `num / den` in lowest terms, so a percentage of 100 is the controller's default share of 1 / 1.
fn lowest_terms(num: u64, den: u64) -> (u64, u64) {
    let (mut a, mut b) = (num, den);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let g = a.max(1);
    (num / g, den / g)
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
        // Unset, the controller's tunables are its own defaults, unchanged.
        assert_eq!(
            settings.tunables(),
            crate::runtime_budget::Tunables::default()
        );
    }

    /// The latency target and the step are the controller's, as set; the rest stay its defaults; 0 is refused.
    #[test]
    fn the_controller_s_latency_target_and_step_are_read_and_judged() {
        let mut vars = MINIMUM.to_vec();
        vars.extend([
            ("CASSINI_BUDGET_LATENCY_PERCENT", "50"),
            ("CASSINI_BUDGET_STEP_BYTES", "67108864"),
        ]);
        assert!(config(&vars).check().is_empty());
        let tunables = BrokerSettings::read(&config(&vars)).unwrap().tunables();
        assert_eq!(tunables.latency_share, (1, 2));
        assert_eq!(tunables.step, 67_108_864);
        assert_eq!(
            crate::runtime_budget::Tunables {
                latency_share: (1, 1),
                step: 16 * 1024 * 1024,
                ..tunables
            },
            crate::runtime_budget::Tunables::default()
        );
        for name in [
            "CASSINI_BUDGET_LATENCY_PERCENT",
            "CASSINI_BUDGET_STEP_BYTES",
        ] {
            let mut vars = MINIMUM.to_vec();
            vars.push((name, "0"));
            let problems = config(&vars).check().join(" | ");
            assert!(
                problems.contains(name) && problems.contains("never grow"),
                "{problems}"
            );
        }
    }

    #[test]
    fn the_budget_and_the_stall_bound_are_read_and_judged() {
        let mut vars = MINIMUM.to_vec();
        vars.extend([
            ("CASSINI_MEMORY_BUDGET_BYTES", "536870912"),
            ("CASSINI_STALL_SECS", "5"),
        ]);
        let settings = BrokerSettings::read(&config(&vars)).unwrap();
        let bounds = settings.bounds().unwrap();
        // The budget starts at its floor, four frames; what is set is its cap.
        assert_eq!(bounds.budget.limit(), 268_435_456);
        assert_eq!(settings.memory_budget, Some(536_870_912));
        assert_eq!(bounds.stall, std::time::Duration::from_secs(5));

        for (name, value, why) in [
            ("CASSINI_MEMORY_BUDGET_BYTES", "1048576", "below its floor"),
            ("CASSINI_STALL_SECS", "60", "TCP_USER_TIMEOUT"),
        ] {
            let mut vars = MINIMUM.to_vec();
            vars.push((name, value));
            let problems = config(&vars).check().join(" | ");
            assert!(problems.contains(why), "{name}={value}: {problems}");
            let settings = BrokerSettings::read(&config(&vars)).unwrap();
            assert!(settings.bounds().is_err() || settings.budget_control().is_err());
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
