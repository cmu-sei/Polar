//! The environment variables the health actor and the liveness probe read, each declared
//! once.
//!
//! The actor and the probe read the same pod environment, so they share these
//! declarations: a malformed `POLAR_HEALTH_STALE_SECS` means the same thing to both. The
//! name of each variable and its default are written here and nowhere else; the constants
//! the rest of the crate (and its tests) use are derived from the declarations.
//!
//! [`SECTION`] groups them with the relation the actor requires between them, for anything
//! that wants to judge an environment before an agent runs in it: the agent builder checks
//! it at startup, and the render-time checker checks the environment a workload will get.

use crate::{DepCertEndpoint, HealthCheckArgs, HealthConfig};
use polar_env::{Defaulted, Described, Env, Optional, Section};

/// Declares a defaulted number of seconds and the constant for its default, from one
/// literal, so the declaration and the constant cannot disagree.
macro_rules! seconds_variable {
    ($(#[$doc:meta])* $default_const:ident, $variable:ident : $ty:ty = $default:literal, $name:literal, $what:literal) => {
        $(#[$doc])*
        pub const $default_const: $ty = $default;
        pub const $variable: Defaulted<$ty> =
            Defaulted::seconds($name, $what, stringify!($default));
    };
}

pub const POLAR_HEALTH_FILE_DEFAULT: &str = "/tmp/polar-health.json";

/// Where the actor writes its state, and the probe reads it.
pub const FILE: Defaulted<String> = Defaulted::text(
    "POLAR_HEALTH_FILE",
    "where the health actor writes its state, which the liveness probe reads",
    POLAR_HEALTH_FILE_DEFAULT,
);

/// The agent's own certificates, watched for expiry.
pub const CERTS: Optional<Vec<String>> = Optional::list(
    "POLAR_HEALTH_CERTS",
    "the agent's own certificate files the health actor watches, comma-separated",
);

/// The services whose certificates must be healthy before this agent rejuvenates.
pub const DEP_ENDPOINTS: Optional<Vec<DepCertEndpoint>> = Optional::new(
    "POLAR_HEALTH_DEP_ENDPOINTS",
    "services whose certificates must be healthy before this agent rejuvenates, comma-separated host:port:min_ttl_seconds",
    "comma-separated host:port:min_ttl_seconds",
    parse_endpoints,
);

/// Not defaulted here: when unset the agent supplies its own, which is
/// [`DEFAULT_REJUVENATION_THRESHOLD_SECS`] for every agent today.
pub const REJUVENATION_SECS: Optional<i64> = Optional::seconds(
    "POLAR_HEALTH_REJUVENATION_SECS",
    "how long before its certificate expires the agent starts to rejuvenate (the agent supplies a default when unset)",
);

/// What every agent passes as its rejuvenation threshold when the deployment says nothing.
pub const DEFAULT_REJUVENATION_THRESHOLD_SECS: i64 = 300;

seconds_variable!(
    DEFAULT_EXPIRY_THRESHOLD_SECS, EXPIRY_SECS: i64 = 60,
    "POLAR_HEALTH_EXPIRY_SECS",
    "the agent is failed outright when its certificate has less than this many seconds left"
);

seconds_variable!(
    DEFAULT_TICK_SECS, TICK_SECS: u64 = 30,
    "POLAR_HEALTH_TICK_SECS",
    "seconds between health ticks"
);

seconds_variable!(
    /// How old the health file may get before the liveness probe fails.
    DEFAULT_STALE_THRESHOLD_SECS, STALE_SECS: u64 = 90,
    "POLAR_HEALTH_STALE_SECS",
    "the liveness probe fails when the health file is older than this many seconds"
);

seconds_variable!(
    /// How long a graph-expecting agent may go without a real successful graph
    /// operation (canary or app-level write) before HealthCheckActor fails itself.
    /// Covers both "never connected" and "was connected, then went dark."
    DEFAULT_GRAPH_DEADLINE_SECS, GRAPH_DEADLINE_SECS: u64 = 120,
    "POLAR_HEALTH_GRAPH_DEADLINE_SECS",
    "seconds an agent that expects a graph may go without a successful graph operation"
);

seconds_variable!(
    /// How long the agent may go without a Cassini connection, from startup or from a
    /// disconnect, before the health actor gives up on it. Chosen against what a broker
    /// restart costs on the dev cluster (measured 2026-10-09, three runs): the broker pod is
    /// back in 24 to 25 s and the clients are registered again 3 to 4 s after that, so an
    /// outage is 27 to 29 s. 120 s is four times that: room for a cold image pull or a slow
    /// certificate issue. An agent that is registered but cannot hear the broker reports
    /// itself disconnected too (the self-test), so this is also how long a deaf agent is
    /// given before it is replaced.
        DEFAULT_CASSINI_DEADLINE_SECS, CASSINI_DEADLINE_SECS: u64 = 120,
    "POLAR_HEALTH_CASSINI_DEADLINE_SECS",
    "seconds the agent may go without a Cassini connection"
);

// The names, for the code and tests that refer to them. Derived from the declarations,
// so each name is written once.
pub const POLAR_HEALTH_FILE_ENV: &str = FILE.name();
pub const POLAR_HEALTH_CERTS_ENV: &str = CERTS.name();
pub const POLAR_HEALTH_DEP_ENDPOINTS_ENV: &str = DEP_ENDPOINTS.name();
pub const POLAR_HEALTH_REJUVENATION_SECS_ENV: &str = REJUVENATION_SECS.name();
pub const POLAR_HEALTH_EXPIRY_SECS_ENV: &str = EXPIRY_SECS.name();
pub const POLAR_HEALTH_TICK_SECS_ENV: &str = TICK_SECS.name();
pub const POLAR_HEALTH_STALE_SECS_ENV: &str = STALE_SECS.name();
pub const POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV: &str = GRAPH_DEADLINE_SECS.name();
pub const POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV: &str = CASSINI_DEADLINE_SECS.name();

fn parse_endpoints(raw: &str) -> Result<Vec<DepCertEndpoint>, String> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            DepCertEndpoint::parse(entry)
                .map_err(|e| format!("has an entry that is not host:port:min_ttl_seconds ({e})"))
        })
        .collect()
}

static VARIABLES: &[&dyn Described] = &[
    &FILE,
    &CERTS,
    &DEP_ENDPOINTS,
    &REJUVENATION_SECS,
    &EXPIRY_SECS,
    &TICK_SECS,
    &STALE_SECS,
    &GRAPH_DEADLINE_SECS,
    &CASSINI_DEADLINE_SECS,
];

/// What must hold between the variables. Run only when each of them parses, so the
/// configuration it builds cannot fail for a reason already reported.
fn relations(env: &dyn Env) -> Vec<String> {
    let args = HealthCheckArgs::new(false, DEFAULT_REJUVENATION_THRESHOLD_SECS, Vec::new());
    match HealthConfig::from_env(args, env) {
        Ok(_) => Vec::new(),
        Err(problem) => vec![problem],
    }
}

/// Everything the health actor and the probe read from the environment.
pub static SECTION: Section = Section {
    name: "health",
    vars: VARIABLES,
    relations,
};
