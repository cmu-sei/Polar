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

//! How an agent's logging and tracing are set up, declared once.
//!
//! `init_logging` used to read three variables of the process environment where it started, so an
//! agent that is given a configuration document (and an environment that holds only a pointer to
//! it) logged at `info` and exported nothing, whatever its deployment wanted. They are declared
//! here as a section every agent has, read from the snapshot the builder took, and
//! [`init_with`] sets logging up from them. `polar::init_logging` is [`init_with`] on the
//! process environment, as it was, for the agents not yet on the builder.

use polar_env::{Config, ConfigError, Defaulted, Described, Optional, Section};

pub const DEFAULT_FILTER: &str = "info";
/// What is exported as traces when the deployment does not say. Only warnings: an `info` span the
/// broker opens for each publish, exported, was 40% of its CPU and held 256 B messages to about
/// 11,000 a second; at `warn` it carried 13,000 to 19,000 (docs/architecture/broker-baseline.md,
/// 2026-10-29). A deployment raises it, per workload, when it wants more.
pub const DEFAULT_TRACE_FILTER: &str = "warn";
pub const DEFAULT_ENDPOINT: &str = "http://localhost:4318/v1/traces";

pub static CONSOLE_FILTER: Defaulted<String> = Defaulted::text(
    "RUST_LOG",
    "what is written to the console, in tracing's filter syntax (for example info, or \
     warn,polar=debug)",
    DEFAULT_FILTER,
)
.dynamic();

pub static TRACE_FILTER: Defaulted<String> = Defaulted::text(
    "POLAR_TRACE_FILTER",
    "which spans and events are exported as traces, in tracing's filter syntax; keep it narrow, \
     since every span it admits is sent",
    DEFAULT_TRACE_FILTER,
)
.dynamic();

pub static OTLP_ENDPOINT: Optional<String> = Optional::text(
    "JAEGER_OTLP_ENDPOINT",
    "where to export traces over OTLP HTTP, for example http://jaeger:4318/v1/traces; none are \
     exported when unset",
);

static VARIABLES: &[&dyn Described] = &[&CONSOLE_FILTER, &TRACE_FILTER, &OTLP_ENDPOINT];

/// The section every agent has: how it logs and traces.
pub static SECTION: Section = Section {
    name: "logging",
    vars: VARIABLES,
    relations: filters_parse,
};

/// A filter that does not parse would silently log nothing it was meant to, so it is a problem
/// before the agent starts.
fn filters_parse(env: &dyn polar_env::Env) -> Vec<String> {
    [&CONSOLE_FILTER, &TRACE_FILTER]
        .into_iter()
        .filter_map(|var| {
            let value = var.read(env).ok()?;
            tracing_subscriber::filter::EnvFilter::try_new(&value)
                .err()
                .map(|e| {
                    format!(
                        "{} is \"{value}\", which is not a log filter: {e}",
                        var.name()
                    )
                })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggingSettings {
    pub console_filter: String,
    pub trace_filter: String,
    /// Traces are exported here, when it is set.
    pub otlp_endpoint: Option<String>,
}

impl LoggingSettings {
    /// From the snapshot an agent was started on.
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            console_filter: config.read(&CONSOLE_FILTER)?,
            trace_filter: config.read(&TRACE_FILTER)?,
            otlp_endpoint: config.read(&OTLP_ENDPOINT)?,
        })
    }

    /// From the process environment, as `init_logging` always read it: `RUST_LOG` filters both,
    /// and traces are exported only when `JAEGER_ENABLE_TRACING` is 1 or true, to
    /// `JAEGER_OTLP_ENDPOINT` or the local collector.
    pub fn from_process_env() -> Self {
        let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| DEFAULT_FILTER.to_string());
        let enabled = std::env::var("JAEGER_ENABLE_TRACING")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Self {
            console_filter: filter.clone(),
            trace_filter: filter,
            otlp_endpoint: enabled.then(|| {
                std::env::var("JAEGER_OTLP_ENDPOINT")
                    .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string())
            }),
        }
    }
}

fn filter_or_default(text: &str) -> tracing_subscriber::filter::EnvFilter {
    tracing_subscriber::filter::EnvFilter::try_new(text)
        .unwrap_or_else(|_| tracing_subscriber::filter::EnvFilter::new(DEFAULT_FILTER))
}

/// Changes a filter that is already in use.
type Reload = Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// How to change the filters of the logging that was set up. One per process, since the
/// subscriber it belongs to is.
struct Handles {
    console: Reload,
    trace: Option<Reload>,
}

static HANDLES: std::sync::OnceLock<Handles> = std::sync::OnceLock::new();

fn reloader<S>(
    handle: tracing_subscriber::reload::Handle<tracing_subscriber::filter::EnvFilter, S>,
) -> Reload
where
    S: 'static,
{
    Box::new(move |text| {
        let filter =
            tracing_subscriber::filter::EnvFilter::try_new(text).map_err(|e| e.to_string())?;
        handle.reload(filter).map_err(|e| e.to_string())
    })
}

/// Changes the filters of the logging that is running: the console's, and the exported traces'
/// when traces are exported. The endpoint cannot change; it is where the exporter was built to
/// send. Fails, changing nothing, if logging was never set up or a filter does not parse.
pub fn reload(settings: &LoggingSettings) -> Result<(), String> {
    let handles = HANDLES
        .get()
        .ok_or("logging has not been set up in this process")?;
    // Both parse first, so one bad filter does not leave the other changed.
    for text in [&settings.console_filter, &settings.trace_filter] {
        tracing_subscriber::filter::EnvFilter::try_new(text.as_str())
            .map_err(|e| format!("`{text}` is not a log filter: {e}"))?;
    }
    (handles.console)(&settings.console_filter)?;
    if let Some(trace) = &handles.trace {
        trace(&settings.trace_filter)?;
    }
    Ok(())
}

/// Sets logging up, and trace export when there is an endpoint. The filters can be changed
/// afterwards with [`reload`].
pub fn init_with(service_name: String, settings: &LoggingSettings) {
    use opentelemetry::{KeyValue, global};
    use opentelemetry_otlp::{Protocol, WithExportConfig};
    use opentelemetry_sdk::{
        Resource,
        trace::{self, RandomIdGenerator},
    };
    use tracing_subscriber::Layer;
    use tracing_subscriber::reload;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    let exporter = settings
        .otlp_endpoint
        .as_deref()
        .filter(|endpoint| !endpoint.is_empty())
        .and_then(|endpoint| {
            opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_protocol(Protocol::HttpBinary)
                .with_endpoint(endpoint)
                .build()
                .ok()
        });

    let (console_filter, console_handle) =
        reload::Layer::new(filter_or_default(&settings.console_filter));

    let (initialised, handles) = if let Some(exporter) = exporter {
        let tracer_provider = trace::SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_sampler(trace::Sampler::AlwaysOn)
            .with_id_generator(RandomIdGenerator::default())
            .with_resource(
                Resource::builder()
                    .with_service_name(service_name.clone())
                    .with_attributes([
                        KeyValue::new("service.name", service_name.clone()),
                        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
                    ])
                    .build(),
            )
            .build();
        global::set_tracer_provider(tracer_provider);
        let (trace_filter, trace_handle) =
            reload::Layer::new(filter_or_default(&settings.trace_filter));
        let telemetry = tracing_opentelemetry::layer()
            .with_tracer(global::tracer("polar"))
            .with_filter(trace_filter);
        let initialised = tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_filter(console_filter))
            .with(telemetry)
            .try_init();
        (
            initialised,
            Handles {
                console: reloader(console_handle),
                trace: Some(reloader(trace_handle)),
            },
        )
    } else {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .pretty()
            .with_thread_ids(true)
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_filter(console_filter);
        (
            tracing_subscriber::registry().with(fmt_layer).try_init(),
            Handles {
                console: reloader(console_handle),
                trace: None,
            },
        )
    };
    match initialised {
        Ok(()) => {
            let _ = HANDLES.set(handles);
        }
        Err(_) => eprintln!("Logging registry already initialized"),
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

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn the_filters_may_change_while_the_agent_runs_and_the_endpoint_may_not() {
        use polar_env::Mutability;
        let mutability = |name: &str| {
            SECTION
                .describe()
                .into_iter()
                .find(|v| v.name == name)
                .unwrap_or_else(|| panic!("{name} is not declared"))
                .mutability
        };
        assert_eq!(mutability("RUST_LOG"), Mutability::Dynamic);
        assert_eq!(mutability("POLAR_TRACE_FILTER"), Mutability::Dynamic);
        // Where traces go is decided when the exporter is built; it needs a restart.
        assert_eq!(mutability("JAEGER_OTLP_ENDPOINT"), Mutability::Static);
    }

    #[test]
    fn reloading_before_logging_is_set_up_says_so_and_changes_nothing() {
        let settings = LoggingSettings {
            console_filter: "warn".to_string(),
            trace_filter: "warn".to_string(),
            otlp_endpoint: None,
        };
        assert!(reload(&settings).is_err());
    }

    #[test]
    fn nothing_set_logs_at_info_admits_only_warnings_to_traces_and_exports_nothing() {
        let settings = LoggingSettings::read(&config(&[])).unwrap();
        assert_eq!(settings.console_filter, "info");
        assert_eq!(settings.trace_filter, "warn");
        assert_eq!(settings.otlp_endpoint, None);
        assert!(config(&[]).check().is_empty());
    }

    #[test]
    fn what_is_set_is_used() {
        let settings = LoggingSettings::read(&config(&[
            ("RUST_LOG", "warn,polar=debug"),
            ("POLAR_TRACE_FILTER", "polar=info"),
            ("JAEGER_OTLP_ENDPOINT", "http://jaeger:4318/v1/traces"),
        ]))
        .unwrap();
        assert_eq!(settings.console_filter, "warn,polar=debug");
        assert_eq!(settings.trace_filter, "polar=info");
        assert_eq!(
            settings.otlp_endpoint.as_deref(),
            Some("http://jaeger:4318/v1/traces")
        );
    }

    #[test]
    fn a_filter_that_does_not_parse_is_named_before_the_agent_starts() {
        for name in ["RUST_LOG", "POLAR_TRACE_FILTER"] {
            let problems = config(&[(name, "not=a=filter[")]).check();
            assert_eq!(problems.len(), 1, "{name}: {problems:?}");
            assert!(
                problems[0].contains(name) && problems[0].contains("not a log filter"),
                "{problems:?}"
            );
        }
    }
}
