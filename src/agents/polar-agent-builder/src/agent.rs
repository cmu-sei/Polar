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

//! What an agent is, as a trait, and the one `main` every agent has.
//!
//! An agent implements [`PolarAgent`]: what it is called and for, what it asks of its
//! deployment, the least time it needs to drain, the variables it reads itself, and the function
//! that starts it. That is the whole of what this infrastructure requires of it, so
//! implementing the trait is hooking into the infrastructure, and the compiler checks the form.
//! Associated constants need no runtime, so [`run`] answers the command line before any exists:
//!
//! - no arguments: judge the configuration, then start the agent;
//! - `--help`: what the agent is, for a person;
//! - `--scaffold`: the configuration document, as JSON: every variable of every section that
//!   applies, with its type, default, secrecy and documentation, and each value empty. With
//!   `--config FILE` it is the same document filled from FILE, with where each value came from
//!   and every problem found;
//! - `--check --config FILE`: the judgement the agent makes when it starts, on FILE, offline.
//!
//! The document an agent reads at runtime is one of these, filled in: see `polar_env::Document`.
//!
//! A complete agent, and one that leaves out what the trait requires, which does not compile:
//!
//! ```
//! use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
//! use std::time::Duration;
//! struct Complete;
//! impl PolarAgent for Complete {
//!     const NAME: &'static str = "complete";
//!     const PURPOSE: &'static str = "says what it needs";
//!     const NEEDS: &'static [Capability] = &[Capability::Cassini];
//!     const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);
//!     async fn start(_: Configured) -> Result<(), StartError> { Ok(()) }
//! }
//! ```
//!
//! ```compile_fail
//! use polar_agent_builder::{Configured, PolarAgent, StartError};
//! struct Incomplete;
//! impl PolarAgent for Incomplete {
//!     const NAME: &'static str = "incomplete";
//!     const PURPOSE: &'static str = "says nothing of what it needs";
//!     async fn start(_: Configured) -> Result<(), StartError> { Ok(()) }
//! }
//! ```

use crate::capability::Capability;
use crate::command::{CommandSpec, spec_problems};
use crate::environment;
use crate::runtime::RuntimeSection;
use crate::startup::{self, Startup};
use crate::topics::{Facts, FamilyDecl, TopicDecl, TopicEntry, TopicSet};
use polar_env::{DocValue, Document, DocumentError, Env, FileEnv, ProcessEnv, Section, Source};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

/// Why an agent could not start, or stopped.
pub type StartError = Box<dyn std::error::Error + Send + Sync>;

/// What an agent is, and what it asks of the infrastructure.
pub trait PolarAgent: Sized + 'static {
    /// The agent's name: its service name, and the name its infrastructure is given.
    const NAME: &'static str;
    /// One line, for a person: what the agent is for.
    const PURPOSE: &'static str;
    /// What it asks of its deployment, beyond the Cassini client, which every agent is and so
    /// need not list. The deployment states what it provides in `POLAR_CAPABILITIES`, and the
    /// agent does not start if one it needs is missing.
    const NEEDS: &'static [Capability];
    /// The least time the agent needs to drain once told to shut down. The deployment may
    /// give more, never less.
    const SHUTDOWN_BUDGET: Duration;
    /// The variables the agent reads itself, declared with `polar-env`. The builder's, the
    /// health actor's and those of the components its needs bring are not listed here.
    const SECTIONS: &'static [&'static Section] = &[];
    /// Longer help than the purpose, for `--help`. Without it a generic text is given.
    const HELP: Option<&'static str> = None;
    /// Whether the agent publishes probes to itself and reports itself disconnected when none
    /// comes back (see `selftest`). On unless the agent has a reason. A service that is not a
    /// Cassini client sets it off.
    const SELF_TEST: bool = true;
    /// Whether this is a Cassini client, as every observer and consumer is. A service that is
    /// not (the broker itself) sets this off and is built with `ServiceBuilder`: it has the
    /// health check, the signals and the shutdown handshake, and no router, client or
    /// subscriptions. It must also turn `SELF_TEST` off and not list `Cassini` in `NEEDS`.
    const CASSINI_CLIENT: bool = true;
    /// Whether the agent's health depends on its Cassini session, as every agent's does. A service that Cassini itself
    /// depends on, and that still speaks over it once it is up (cert-issuer: docs/architecture/cert-issuer-isolation.md,
    /// "The model", revised 2026-10-23), sets this off: its client connects, reconnects and passes the topic gate as
    /// any other, and a lost session, or one never made, is reported and never marks it unhealthy or fails it.
    const DEPENDS_ON_CASSINI: bool = true;
    /// Capabilities the agent uses when its deployment provides them and does without when it
    /// does not, so one binary can run as part of a cluster or on its own. Only `Cassini` may be
    /// optional so far, and then `CASSINI_CLIENT` must be off: the agent is a Cassini client
    /// exactly when `POLAR_CAPABILITIES` lists `cassini`. A deployment that says nothing (the
    /// variable is unset) provides nothing optional, so an agent started with no cluster around
    /// it runs on its own. `Configured::cassini_enabled` says which way it went, and `start`
    /// builds with `AgentBuilder` or with `ServiceBuilder` to match.
    const OPTIONAL: &'static [Capability] = &[];
    /// The commands the agent accepts (docs/architecture/commands.md): each with its arguments, how long it
    /// may run and how the scheduler may deliver it. They appear in the schema export, and the agent
    /// registers a handler for each with `AgentBuilder::with_command`; it does not start if the two
    /// disagree. A command not declared here is refused however it is signed.
    const COMMANDS: &'static [CommandSpec] = &[];
    /// The sections of runtime configuration the agent accepts (docs/architecture/git-controller.md): what
    /// it works on, as opposed to how it runs. Each is replaced whole by a signed `runtime` document from
    /// someone holding `c:update-runtime`, judged by the section's own check, and read with
    /// `ConfigurationHandle::runtime`. They appear in the schema export beside the variable sections. A
    /// section not declared here is refused however it is signed.
    const RUNTIME: &'static [&'static RuntimeSection] = &[];
    /// The topics the agent uses itself, each with its access and its purpose (docs/architecture/topics.md).
    /// The topics every agent uses, and those a need brings, are implied and not listed here; the scaffold
    /// prints the whole set, and the agent's client refuses what is outside it unless policy permits it.
    const TOPICS: &'static [TopicDecl] = &[];
    /// The topics the agent uses once for each workload it serves (the scheduler's `agent.{workload}.configuration`).
    /// Each is expanded into exact names from `POLAR_SERVED_WORKLOADS`, which the render fills from the catalog;
    /// an agent that declares one reads that variable.
    const TOPIC_FAMILIES: &'static [FamilyDecl] = &[];

    /// The agent proper. It is given the configuration the infrastructure judged, and builds
    /// its infrastructure from it (see `AgentBuilder::into_configured`). It returns when the
    /// agent ends; `run` then exits non-zero, because an agent that ends has failed.
    fn start(configured: Configured) -> impl Future<Output = Result<(), StartError>> + Send;
}

/// The configuration an agent was started with, already judged: the snapshot, the health
/// actor's settings and the shutdown budget, and what the agent declared.
pub struct Configured {
    pub(crate) startup: Startup,
    pub(crate) required: Vec<Capability>,
    /// The optional capabilities the deployment did not provide, which the agent does without.
    pub(crate) optional_off: Vec<Capability>,
    pub(crate) self_test: bool,
    /// What the agent declared it accepts.
    pub(crate) commands: &'static [CommandSpec],
    /// The runtime sections the agent declared.
    pub(crate) runtime: &'static [&'static RuntimeSection],
    /// Every topic the agent uses, its families expanded for the workloads it serves.
    pub(crate) topics: Arc<TopicSet>,
    pub(crate) hooks: Hooks,
}

/// What a test replaces about how an agent ends: the real process exit, the signal listener.
#[derive(Clone)]
pub(crate) struct Hooks {
    pub(crate) process_exit: crate::shutdown::ProcessExit,
    pub(crate) listen_for_signals: bool,
    /// Where `ConfiguredService::build` leaves the resources it made, for a test to hold.
    pub(crate) resources_out: Option<Arc<std::sync::Mutex<Option<crate::ServiceResources>>>>,
}

impl Hooks {
    pub(crate) fn real() -> Self {
        Self {
            process_exit: crate::shutdown::real_process_exit(),
            listen_for_signals: true,
            resources_out: None,
        }
    }
}

impl Configured {
    /// The snapshot. Read the agent's own settings from it with `config.read(&VAR)`.
    pub fn config(&self) -> &Arc<polar_env::Config> {
        &self.startup.config
    }

    pub fn shutdown_budget(&self) -> Duration {
        self.startup.shutdown_budget
    }

    /// Every topic the agent may use: declared, implied, and its families expanded for the workloads it
    /// serves. The agent's client refuses anything else unless its policy permits it.
    pub fn topics(&self) -> &Arc<TopicSet> {
        &self.topics
    }

    /// Whether the agent is a Cassini client in this deployment: always for an agent, and for one
    /// whose Cassini is optional, exactly when the deployment provides it. Build with
    /// `AgentBuilder` when it is and with `ServiceBuilder` when it is not.
    pub fn cassini_enabled(&self) -> bool {
        self.required.contains(&Capability::Cassini)
    }

    /// Sets logging up as the deployment says (`RUST_LOG`, `POLAR_TRACE_FILTER`,
    /// `JAEGER_OTLP_ENDPOINT` in the snapshot), and starts exporting traces when an endpoint is
    /// given. Call it first in `start`. It cannot fail: the snapshot was judged before this ran.
    pub fn init_logging(&self, service_name: &str) {
        match polar::logging::LoggingSettings::read(&self.startup.config) {
            Ok(settings) => polar::logging::init_with(service_name.to_string(), &settings),
            Err(error) => {
                // Unreachable once the snapshot has been judged; log with the defaults.
                eprintln!("ERROR: cannot read the logging settings: {error}");
                polar::init_logging(service_name.to_string());
            }
        }
        for capability in &self.optional_off {
            tracing::info!(
                "running without {capability}: the deployment did not provide it ({} does not \
                 list it)",
                crate::capability::PROVIDED_ENV
            );
        }
    }

    /// For the tests of a service built on [`crate::ServiceBuilder`]: a configuration judged
    /// from `env` with no capabilities required, that records the codes the process was asked
    /// to exit with instead of exiting, does not listen for signals (they belong to the whole
    /// process), and leaves the resources the builder makes where the test can reach them.
    #[doc(hidden)]
    pub fn for_service_test(
        env: &dyn Env,
        budget: Duration,
        exits: Arc<std::sync::Mutex<Vec<i32>>>,
    ) -> (Self, Arc<std::sync::Mutex<Option<crate::ServiceResources>>>) {
        let startup = startup::prepare(env, &[], &[], Some(budget))
            .unwrap_or_else(|problems| panic!("the test's environment is not sound: {problems}"));
        let out = Arc::new(std::sync::Mutex::new(None));
        let configured = Self {
            startup,
            required: Vec::new(),
            optional_off: Vec::new(),
            self_test: false,
            commands: &[],
            runtime: &[],
            topics: Arc::new(TopicSet::default()),
            hooks: Hooks {
                process_exit: Arc::new(move |code| exits.lock().unwrap().push(code)),
                listen_for_signals: false,
                resources_out: Some(out.clone()),
            },
        };
        (configured, out)
    }
}

// -- the document --------------------------------------------------------------------------------

fn depends() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// The configuration document of one agent: the sections that apply to it, and what it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDocument {
    pub agent: AgentInfo,
    /// The capabilities the agent's code requires.
    pub requires: Vec<String>,
    /// The capabilities it uses when the deployment provides them and does without otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional: Vec<String>,
    /// Whether the agent's health depends on its Cassini session ([`PolarAgent::DEPENDS_ON_CASSINI`]): the catalog
    /// leaves Cassini out of the health dependencies of one that does not. Written only when it does not, so every
    /// other agent's scaffold is as it was; a scaffold without it depends on Cassini.
    #[serde(default = "depends", skip_serializing_if = "is_true")]
    pub depends_on_cassini: bool,
    /// The least the deployment may give the agent to drain, in seconds.
    pub shutdown_budget_minimum_secs: u64,
    /// The commands the agent accepts, with their arguments: what a signed command may be.
    #[serde(default)]
    pub commands: Vec<CommandDocument>,
    /// The sections of runtime configuration the agent accepts, with what each body may hold: beside
    /// `sections`, which are the variables.
    #[serde(default)]
    pub runtime: Vec<RuntimeDocument>,
    /// Every topic the agent uses, declared or implied, each with where it comes from: the one computed
    /// set, which the catalog and the certificates take from here and never recompute. A family appears here
    /// expanded for the workloads in the document the scaffold was filled from, and in `topic_families` as
    /// declared.
    #[serde(default)]
    pub topics: Vec<TopicEntry>,
    /// The groups the agent is in, which decide its implied topics: the catalog writes them into its floor, and its
    /// certificate carries them as `g:` names.
    #[serde(default)]
    pub groups: Vec<String>,
    /// The agent's topic families, unexpanded: what `POLAR_SERVED_WORKLOADS` expands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topic_families: Vec<FamilyDocument>,
    #[serde(flatten)]
    pub document: Document,
    /// Every problem found, when the document was filled from a file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

/// A topic family an agent declares, as the scaffold shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyDocument {
    pub pattern: String,
    pub access: crate::topics::Access,
    pub purpose: String,
    /// The variable whose workloads it is expanded for.
    pub over: String,
}

/// A runtime section an agent accepts, as the schema export shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeDocument {
    pub name: String,
    pub doc: String,
    /// What the body may hold, field by field, as the agent declared it.
    pub body: serde_json::Value,
}

/// A command an agent accepts, as the schema export shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDocument {
    pub name: String,
    pub doc: String,
    pub args: Vec<ArgDocument>,
    /// `once` or `idempotent`.
    pub delivery: String,
    pub exclusive: bool,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgDocument {
    pub name: String,
    /// `text` or `number`.
    pub kind: String,
    pub required: bool,
    pub doc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub purpose: String,
}

/// What `A` needs, with the Cassini client first: every agent built here is one, so the agent
/// does not have to say so and cannot forget to.
pub(crate) fn needs_of<A: PolarAgent>() -> Vec<Capability> {
    let mut needs = if A::CASSINI_CLIENT {
        vec![Capability::Cassini]
    } else {
        Vec::new()
    };
    for capability in A::NEEDS {
        if !needs.contains(capability) {
            needs.push(*capability);
        }
    }
    needs
}

/// What `A` uses in a deployment that provides `provided` (the value of `POLAR_CAPABILITIES`, if
/// it is set): what it needs, and the optional capabilities the deployment lists. A deployment
/// that says nothing provides nothing optional.
pub(crate) fn effective_of<A: PolarAgent>(provided: Option<&str>) -> Vec<Capability> {
    let mut used = needs_of::<A>();
    let listed = provided.map(Capability::parse_list).unwrap_or_default();
    for capability in A::OPTIONAL {
        if listed.contains(capability) && !used.contains(capability) {
            used.push(*capability);
        }
    }
    used
}

/// Whether `A` does without the Cassini client when its deployment does not provide one.
fn cassini_optional<A: PolarAgent>() -> bool {
    A::OPTIONAL.contains(&Capability::Cassini)
}

/// What the topic rules read about `A`. An agent that may run without the broker uses its topics when it
/// has one, so its set is the one it has then.
fn topic_facts<A: PolarAgent>() -> Facts<'static> {
    Facts {
        name: A::NAME,
        cassini_client: A::CASSINI_CLIENT || cassini_optional::<A>(),
        self_test: A::SELF_TEST,
        encryption: A::NEEDS.contains(&Capability::Encryption),
    }
}

/// The whole set of topics `A` uses: what it declares and what is implied for it. Its families serve no
/// workload here; see [`topic_set_serving`].
pub fn topic_set<A: PolarAgent>() -> TopicSet {
    topic_set_serving::<A>(&[])
}

/// As [`topic_set`], with `A`'s families expanded for each of `workloads`.
pub fn topic_set_serving<A: PolarAgent>(workloads: &[String]) -> TopicSet {
    TopicSet::compute_with(&topic_facts::<A>(), A::TOPICS, A::TOPIC_FAMILIES, workloads)
}

/// The workloads `A` serves, from its configuration; none for an agent that declares no family.
fn served_by<A: PolarAgent>(config: &polar_env::Config) -> Result<Vec<String>, String> {
    if A::TOPIC_FAMILIES.is_empty() {
        return Ok(Vec::new());
    }
    let text = config
        .read(&crate::topics::SERVED_WORKLOADS)
        .map_err(|e| e.to_string())?;
    Ok(crate::topics::served_workloads(&text))
}

/// The variables `A` reads itself, and the builder's for its topic families when it declares any.
fn own_sections<A: PolarAgent>() -> Vec<&'static Section> {
    let mut sections: Vec<&'static Section> = A::SECTIONS.to_vec();
    if !A::TOPIC_FAMILIES.is_empty() {
        sections.push(&crate::topics::FAMILY_SECTION);
    }
    sections
}

/// The sections that apply to `A`, each with the capability that brings it in (none for the
/// ones every agent has, and for its own).
fn sections_of<A: PolarAgent>() -> Vec<(Option<Capability>, &'static Section)> {
    let mut sections: Vec<(Option<Capability>, &'static Section)> = environment::base_sections()
        .into_iter()
        .map(|section| (None, section))
        .collect();
    for capability in needs_of::<A>() {
        sections.extend(
            environment::sections_for(&[capability])
                .into_iter()
                .map(|section| (Some(capability), section)),
        );
    }
    // The sections of the optional capabilities are in the scaffold, so a deployment that
    // provides them can fill them; they are judged only when it does (see `effective_of`).
    for capability in A::OPTIONAL {
        sections.extend(
            environment::sections_for(&[*capability])
                .into_iter()
                .map(|section| (Some(*capability), section)),
        );
    }
    sections.extend(
        own_sections::<A>()
            .into_iter()
            .map(|section| (None, section)),
    );
    sections
}

/// The scaffold of `A`: every value empty.
pub fn scaffold_document<A: PolarAgent>() -> AgentDocument {
    let tagged: Vec<(&Section, Option<&str>)> = sections_of::<A>()
        .into_iter()
        .map(|(capability, section)| (section, capability.map(Capability::as_str)))
        .collect();
    AgentDocument {
        agent: AgentInfo {
            name: A::NAME.to_string(),
            purpose: A::PURPOSE.to_string(),
        },
        requires: needs_of::<A>()
            .iter()
            .map(|c| c.as_str().to_string())
            .collect(),
        optional: A::OPTIONAL.iter().map(|c| c.as_str().to_string()).collect(),
        depends_on_cassini: A::DEPENDS_ON_CASSINI,
        shutdown_budget_minimum_secs: A::SHUTDOWN_BUDGET.as_secs(),
        commands: A::COMMANDS
            .iter()
            .map(|c| CommandDocument {
                name: c.name.to_string(),
                doc: c.doc.to_string(),
                args: c
                    .args
                    .iter()
                    .map(|a| ArgDocument {
                        name: a.name.to_string(),
                        kind: a.kind.as_str().to_string(),
                        required: a.required,
                        doc: a.doc.to_string(),
                    })
                    .collect(),
                delivery: c.delivery.as_str().to_string(),
                exclusive: c.exclusive,
                timeout_secs: c.timeout.as_secs(),
            })
            .collect(),
        runtime: A::RUNTIME
            .iter()
            .map(|section| RuntimeDocument {
                name: section.name.to_string(),
                doc: section.doc.to_string(),
                // A body description that is not JSON is a declaration problem, reported by
                // `declaration_problems`; the export shows null for it.
                body: serde_json::from_str(section.body).unwrap_or(serde_json::Value::Null),
            })
            .collect(),
        topics: topic_set::<A>().entries(),
        groups: crate::topics::groups_for(&topic_facts::<A>())
            .into_iter()
            .map(str::to_string)
            .collect(),
        topic_families: A::TOPIC_FAMILIES
            .iter()
            .map(|f| FamilyDocument {
                pattern: f.pattern.to_string(),
                access: f.access,
                purpose: f.purpose.to_string(),
                over: crate::topics::SERVED_WORKLOADS.name().to_string(),
            })
            .collect(),
        document: Document::scaffold(&tagged),
        problems: Vec::new(),
    }
}

/// A document read from a file, and the environment it makes.
struct Loaded {
    document: Document,
    env: FileEnv,
}

fn load(path: &Path, process: impl Env + 'static) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| DocumentError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        })
        .map_err(|e| e.to_string())?;
    let document = Document::from_json(&text).map_err(|e| e.to_string())?;
    let env = FileEnv::new(&document, process);
    Ok(Loaded { document, env })
}

/// Every problem with a document, as `A` would find them starting on it: what is wrong with
/// the document itself, what it sets that the agent does not declare, and everything the
/// agent's own judgement of its configuration finds.
/// What is wrong with what the agent offers to signed documents, its commands and its runtime sections, and
/// with the topics it declares, whatever the configuration.
fn surface_problems<A: PolarAgent>() -> Vec<String> {
    let mut problems = spec_problems(A::COMMANDS);
    if !A::COMMANDS.is_empty() && !A::CASSINI_CLIENT {
        problems.push(format!(
            "{} declares commands but is not a Cassini client, so none can reach it",
            A::NAME
        ));
    }
    problems.extend(crate::runtime::check_declarations(A::RUNTIME));
    problems.extend(crate::topics::problems(&topic_facts::<A>(), A::TOPICS));
    problems.extend(crate::topics::family_problems(A::TOPIC_FAMILIES));
    if !A::RUNTIME.is_empty() && !A::CASSINI_CLIENT {
        problems.push(format!(
            "{} declares runtime sections but is not a Cassini client, so no document can reach it",
            A::NAME
        ));
    }
    problems
}

fn problems_of<A: PolarAgent>(loaded: &Loaded) -> Vec<String> {
    let mut problems = loaded.document.problems();
    problems.extend(surface_problems::<A>());

    let declared: Vec<String> = scaffold_document::<A>()
        .document
        .sections
        .iter()
        .flat_map(|s| s.variables.iter().map(|v| v.name.clone()))
        .collect();
    for variable in loaded
        .document
        .sections
        .iter()
        .flat_map(|s| s.variables.iter())
    {
        if variable.value.is_some() && !declared.contains(&variable.name) {
            problems.push(format!(
                "{} is given a value in the document, but {} does not declare it",
                variable.name,
                A::NAME
            ));
        }
    }

    if let Err(error) = startup::prepare_from(
        &loaded.env,
        Source::File,
        &own_sections::<A>(),
        &effective_of::<A>(loaded.env.var(crate::capability::PROVIDED_ENV).as_deref()),
        Some(A::SHUTDOWN_BUDGET),
    ) {
        problems.extend(error.problems());
    }
    problems
}

/// The scaffold of `A` as JSON; with `config`, filled from that file, with sources, resolved
/// values and every problem found. `Err` when the file cannot be read as a document.
pub fn scaffold_json<A: PolarAgent>(
    config: Option<&Path>,
    process: impl Env + 'static,
) -> Result<String, String> {
    let mut document = scaffold_document::<A>();
    if config.is_none() && !A::TOPIC_FAMILIES.is_empty() {
        // Asked with no document, a family is expanded for the workloads the environment names, which is how
        // the render asks it, with the list it writes into the agent's document.
        let env = polar_env::Config::builder(&process)
            .register(&crate::topics::FAMILY_SECTION)
            .build();
        if let Ok(served) = served_by::<A>(&env) {
            document.topics = topic_set_serving::<A>(&served).entries();
        }
    }
    if let Some(path) = config {
        let loaded = load(path, process)?;
        document.problems = problems_of::<A>(&loaded);

        let mut builder = polar_env::Config::builder(&loaded.env).source(Source::File);
        for (_, section) in sections_of::<A>() {
            builder = builder.register(section);
        }
        let filled = builder.build();
        let report = filled.report();
        if let Ok(served) = served_by::<A>(&filled) {
            document.topics = topic_set_serving::<A>(&served).entries();
        }

        let written: std::collections::HashMap<&str, &DocValue> = loaded
            .document
            .sections
            .iter()
            .flat_map(|s| s.variables.iter())
            .filter_map(|v| Some((v.name.as_str(), v.value.as_ref()?)))
            .collect();
        for variable in document
            .document
            .sections
            .iter_mut()
            .flat_map(|s| s.variables.iter_mut())
        {
            variable.value = written.get(variable.name.as_str()).map(|v| (*v).clone());
            if let Some(resolved) = report.iter().find(|r| r.name == variable.name) {
                variable.source = Some(resolved.source.as_str().to_string());
                variable.resolved = resolved.value.clone();
            }
        }
    }
    serde_json::to_string_pretty(&document).map_err(|e| e.to_string())
}

/// Every problem with the document at `config`, as the agent would find them starting on it.
pub fn check<A: PolarAgent>(config: &Path, process: impl Env + 'static) -> Vec<String> {
    match load(config, process) {
        Ok(loaded) => problems_of::<A>(&loaded),
        Err(error) => vec![error],
    }
}

// -- the command line -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run,
    Help,
    Scaffold { config: Option<PathBuf> },
    Check { config: PathBuf },
}

const USAGE: &str = "usage: <agent> [--help | --scaffold [--config FILE] | --check --config FILE]";

pub fn parse_command(args: &[String]) -> Result<Command, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let config_of = |rest: &[&str]| -> Result<Option<PathBuf>, String> {
        match rest {
            [] => Ok(None),
            ["--config", path] => Ok(Some(PathBuf::from(path))),
            ["--config"] => Err(format!("--config needs a file\n{USAGE}")),
            _ => Err(USAGE.to_string()),
        }
    };
    match args.as_slice() {
        [] => Ok(Command::Run),
        ["--help"] | ["-h"] => Ok(Command::Help),
        ["--scaffold", rest @ ..] => Ok(Command::Scaffold {
            config: config_of(rest)?,
        }),
        ["--check", rest @ ..] => match config_of(rest)? {
            Some(config) => Ok(Command::Check { config }),
            None => Err(format!("--check needs --config FILE\n{USAGE}")),
        },
        _ => Err(USAGE.to_string()),
    }
}

/// What `--help` prints for `A`.
pub fn help_text<A: PolarAgent>() -> String {
    let mut text = format!("{}\n\n{}\n\n", A::NAME, A::PURPOSE);
    match A::HELP {
        Some(help) => text.push_str(&format!("{}\n\n", help.trim())),
        None if cassini_optional::<A>() => text.push_str(&format!(
            "{} is a Polar service that runs on its own or as part of a cluster. When its \
             deployment provides the cassini capability (POLAR_CAPABILITIES lists it) it connects \
             to the Cassini broker as a client with its own certificate; otherwise it does \
             without. It reports its health and drains before it exits. Its configuration is a \
             document (see --scaffold); a value it does not find there it takes from the default \
             its declaration gives.\n\n",
            A::NAME
        )),
        None if A::CASSINI_CLIENT => text.push_str(&format!(
            "{} is a Polar agent: it connects to the Cassini broker as a client with its own \
             certificate, reports its health, and drains before it exits. Its configuration \
             is a document (see --scaffold); a value it does not find there it takes from \
             the default its declaration gives.\n\n",
            A::NAME
        )),
        None => text.push_str(&format!(
            "{} is a Polar service, not a Cassini client: it reports its health and drains \
             before it exits, and has no connection of its own to the broker. Its configuration \
             is a document (see --scaffold); a value it does not find there it takes from \
             the default its declaration gives.\n\n",
            A::NAME
        )),
    }
    let names: Vec<&str> = needs_of::<A>().iter().map(|c| c.as_str()).collect();
    if A::CASSINI_CLIENT {
        text.push_str(&format!(
            "Capabilities it needs from its deployment: {} (cassini is implied: every agent is \
             a Cassini client)\n",
            names.join(", ")
        ));
    } else {
        text.push_str(&format!(
            "Capabilities it needs from its deployment: {} (it is not always a Cassini client)\n",
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        ));
    }
    if !A::OPTIONAL.is_empty() {
        let optional: Vec<&str> = A::OPTIONAL.iter().map(|c| c.as_str()).collect();
        text.push_str(&format!(
            "Capabilities it uses when its deployment provides them: {}\n",
            optional.join(", ")
        ));
    }
    text.push_str(&format!(
        "Shutdown: it needs at least {} seconds to drain.\n\n",
        A::SHUTDOWN_BUDGET.as_secs()
    ));
    if !A::SECTIONS.is_empty() {
        text.push_str("Variables it reads itself:\n");
        for section in A::SECTIONS {
            for variable in section.describe() {
                let how = match variable.kind {
                    polar_env::Kind::Required => "required".to_string(),
                    polar_env::Kind::Optional => "optional".to_string(),
                    polar_env::Kind::Default => {
                        format!("default {}", variable.default.unwrap_or_default())
                    }
                };
                let secret = if variable.secret { ", secret" } else { "" };
                text.push_str(&format!(
                    "  {}  [{how}{secret}]  {}\n",
                    variable.name, variable.doc
                ));
            }
        }
        text.push('\n');
    }
    text.push_str(
        "The builder's, the health actor's and those of its capabilities' components are in the \
         scaffold.\n\n\
         Flags:\n\
         \x20 --help                    this text\n\
         \x20 --scaffold                the configuration document, every value empty, as JSON\n\
         \x20 --scaffold --config FILE  the same, filled from FILE, with sources and problems\n\
         \x20 --check --config FILE     judge FILE the way the agent does when it starts\n\
         With no flag it starts the agent, reading POLAR_CONFIG if it is set and the environment \
         otherwise.\n",
    );
    text
}

/// What is wrong with how `A` declared itself, apart from any configuration. An agent crate runs
/// this in its tests, through `polar_agent_tests!`.
pub fn declaration_problems<A: PolarAgent>() -> Vec<String> {
    let mut problems = Vec::new();
    if A::NAME.trim().is_empty() {
        problems.push("the agent has no name".to_string());
    }
    if A::PURPOSE.trim().is_empty() {
        problems.push("the agent has no purpose: say in a line what it is for".to_string());
    }
    if A::SHUTDOWN_BUDGET < Duration::from_secs(1) {
        problems.push("the shutdown budget is under a second".to_string());
    }
    if !A::CASSINI_CLIENT {
        if A::SELF_TEST && !cassini_optional::<A>() {
            problems.push(
                "the agent is not a Cassini client but SELF_TEST is on: a service has no broker \
                 to publish its probes to; set SELF_TEST = false"
                    .to_string(),
            );
        }
        if A::NEEDS.contains(&Capability::Cassini) {
            problems.push(
                "the agent is not a Cassini client (CASSINI_CLIENT = false) but lists Cassini in \
                 NEEDS"
                    .to_string(),
            );
        }
    }
    problems.extend(surface_problems::<A>());
    let mut optional_seen: Vec<Capability> = Vec::new();
    for capability in A::OPTIONAL {
        if *capability != Capability::Cassini {
            problems.push(format!(
                "{} cannot be optional: only cassini can be so far",
                capability.as_str()
            ));
        }
        if A::NEEDS.contains(capability) {
            problems.push(format!(
                "{} is listed both in NEEDS and in OPTIONAL",
                capability.as_str()
            ));
        }
        if optional_seen.contains(capability) {
            problems.push(format!(
                "{} is listed more than once in OPTIONAL",
                capability.as_str()
            ));
        }
        optional_seen.push(*capability);
    }
    if cassini_optional::<A>() && A::CASSINI_CLIENT {
        problems.push(
            "cassini is optional but CASSINI_CLIENT is on: an agent that is always a client cannot \
             do without it; set CASSINI_CLIENT = false"
                .to_string(),
        );
    }
    let mut seen: Vec<Capability> = Vec::new();
    for capability in A::NEEDS {
        if seen.contains(capability) {
            problems.push(format!(
                "{} is listed more than once in NEEDS",
                capability.as_str()
            ));
        }
        seen.push(*capability);
    }
    let mut names: Vec<&str> = Vec::new();
    let mut sections: Vec<&str> = Vec::new();
    for (_, section) in sections_of::<A>() {
        if sections.contains(&section.name) {
            problems.push(format!(
                "a section named {} is declared more than once",
                section.name
            ));
        }
        sections.push(section.name);
        for variable in section.describe() {
            if names.contains(&variable.name) {
                problems.push(format!("{} is declared more than once", variable.name));
            }
            names.push(variable.name);
        }
        problems.extend(section.defaults_problems());
    }
    problems
}

/// Declares the test every agent crate runs on its declaration.
#[macro_export]
macro_rules! polar_agent_tests {
    ($agent:ty) => {
        #[test]
        fn the_agent_declares_itself_soundly() {
            let problems = $crate::agent::declaration_problems::<$agent>();
            assert!(problems.is_empty(), "{problems:#?}");
        }
    };
}

/// The one `main`: answers the command line, judges the configuration, and starts the agent.
/// The process exit status is 0 only for a command that was answered; an agent that was started
/// never ends well, and exits 1 whether `start` returned an error or not.
pub fn run<A: PolarAgent>() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(run_with::<A>(&args, ProcessEnv))
}

/// As [`run`], with the arguments and the process environment given, returning the exit status.
pub fn run_with<A: PolarAgent>(args: &[String], process: impl Env + 'static) -> u8 {
    let command = match parse_command(args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{message}");
            return 2;
        }
    };
    match command {
        Command::Help => {
            print!("{}", help_text::<A>());
            0
        }
        Command::Scaffold { config } => match scaffold_json::<A>(config.as_deref(), process) {
            Ok(text) => {
                println!("{text}");
                0
            }
            Err(error) => {
                eprintln!("ERROR: {error}");
                1
            }
        },
        Command::Check { config } => {
            let problems = check::<A>(&config, process);
            if problems.is_empty() {
                println!("ok: the configuration is sound");
                0
            } else {
                for problem in &problems {
                    eprintln!("ERROR: {problem}");
                }
                1
            }
        }
        Command::Run => start::<A>(process),
    }
}

/// Judges the configuration the process was given and, if it is sound, runs the agent.
fn start<A: PolarAgent>(process: impl Env + 'static) -> u8 {
    let (env, source) = match polar_env::from_environment(process) {
        Ok(chosen) => chosen,
        Err(error) => {
            eprintln!("ERROR: {error}");
            return 1;
        }
    };
    // What the agent uses here: what it needs, and the optional capabilities the deployment
    // provides. A deployment that does not say runs the agent on its own.
    let used = effective_of::<A>(env.var(crate::capability::PROVIDED_ENV).as_deref());
    let mut startup = match startup::prepare_from(
        &*env,
        source,
        &own_sections::<A>(),
        &used,
        Some(A::SHUTDOWN_BUDGET),
    ) {
        Ok(startup) => startup,
        Err(error) => {
            for problem in error.problems() {
                eprintln!("ERROR: {problem}");
            }
            return 1;
        }
    };
    let bad_commands = surface_problems::<A>();
    if !bad_commands.is_empty() {
        for problem in &bad_commands {
            eprintln!("ERROR: {problem}");
        }
        return 1;
    }
    let served = match served_by::<A>(&startup.config) {
        Ok(served) => served,
        Err(error) => {
            eprintln!("ERROR: {error}");
            return 1;
        }
    };
    startup.health.expects_cassini = A::DEPENDS_ON_CASSINI;
    let configured = Configured {
        topics: Arc::new(topic_set_serving::<A>(&served)),
        startup,
        optional_off: A::OPTIONAL
            .iter()
            .filter(|capability| !used.contains(capability))
            .copied()
            .collect(),
        self_test: A::SELF_TEST && used.contains(&Capability::Cassini),
        required: used,
        commands: A::COMMANDS,
        runtime: A::RUNTIME,
        hooks: Hooks::real(),
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("ERROR: cannot start the async runtime: {error}");
            return 1;
        }
    };
    match runtime.block_on(A::start(configured)) {
        Ok(()) => eprintln!(
            "{} ended; an agent that ends has failed, exiting 1",
            A::NAME
        ),
        Err(error) => eprintln!("{} failed: {error}", A::NAME),
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::fixtures::{CASSINI_ENV, GRAPH_ENV};
    use polar_env::{Described, Required};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    static BUCKET: Required<String> =
        Required::text("EXAMPLE_BUCKET", "the bucket the example writes to");
    static TOKEN: Required<String> =
        Required::text("EXAMPLE_TOKEN", "the example's token").secret();
    static OWN_VARS: &[&dyn Described] = &[&BUCKET, &TOKEN];
    static OWN: Section = Section {
        name: "example",
        vars: OWN_VARS,
        relations: |_| Vec::new(),
    };

    static STARTED: AtomicBool = AtomicBool::new(false);

    struct Example;

    impl PolarAgent for Example {
        const NAME: &'static str = "example-agent";
        const PURPOSE: &'static str = "Copies things from one place to another";
        const NEEDS: &'static [Capability] = &[Capability::Cassini, Capability::GraphWrite];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(30);
        const SECTIONS: &'static [&'static Section] = &[&OWN];
        const HELP: Option<&'static str> = Some("It reads from the bucket and writes the graph.");
        const TOPICS: &'static [TopicDecl] = &[
            TopicDecl::subscribe("bucket.changes", "what changed in the bucket"),
            TopicDecl::publish("bucket.copied", "what it copied")
                .because(&[crate::topics::Cause::Topic("bucket.changes")]),
        ];

        async fn start(configured: Configured) -> Result<(), StartError> {
            // It reads its own settings from the snapshot it was given.
            let bucket = configured.config().read(&BUCKET)?;
            assert_eq!(bucket, "the-bucket");
            STARTED.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// An agent that notes that it was started, apart from `Example`, so tests that run at the
    /// same time do not see each other's.
    static WATCHED_STARTED: AtomicBool = AtomicBool::new(false);

    struct Watched;

    impl PolarAgent for Watched {
        const NAME: &'static str = "watched-agent";
        const PURPOSE: &'static str = "Notes that it was started";
        const NEEDS: &'static [Capability] = &[Capability::Cassini];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
        async fn start(_: Configured) -> Result<(), StartError> {
            WATCHED_STARTED.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct Bare;

    impl PolarAgent for Bare {
        const NAME: &'static str = "bare-agent";
        const PURPOSE: &'static str = "Needs nothing";
        const NEEDS: &'static [Capability] = &[];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    /// A service that is not a Cassini client, as the broker is.
    struct Serving;

    impl PolarAgent for Serving {
        const NAME: &'static str = "serving";
        const PURPOSE: &'static str = "Serves instead of consuming";
        const NEEDS: &'static [Capability] = &[];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);
        const SELF_TEST: bool = false;
        const CASSINI_CLIENT: bool = false;
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    /// One that says it is not a client but forgot the rest of what that means.
    struct Muddled;

    impl PolarAgent for Muddled {
        const NAME: &'static str = "muddled";
        const PURPOSE: &'static str = "Contradicts itself";
        const NEEDS: &'static [Capability] = &[Capability::Cassini];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);
        const CASSINI_CLIENT: bool = false;
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    /// One that serves other workloads, as the scheduler does.
    struct Scheduling;

    impl PolarAgent for Scheduling {
        const NAME: &'static str = "scheduling";
        const PURPOSE: &'static str = "Publishes each workload's configuration";
        const NEEDS: &'static [Capability] = &[];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
        const TOPIC_FAMILIES: &'static [FamilyDecl] = &[FamilyDecl::publish(
            "agent.{workload}.configuration",
            "each workload's documents",
        )];
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    /// One that declares a topic it is given anyway.
    struct Redundant;

    impl PolarAgent for Redundant {
        const NAME: &'static str = "redundant";
        const PURPOSE: &'static str = "Declares what is implied";
        const NEEDS: &'static [Capability] = &[];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
        const TOPICS: &'static [TopicDecl] = &[TopicDecl::subscribe(
            "agent.redundant.configuration",
            "its configuration",
        )];
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    crate::polar_agent_tests!(Example);

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    /// The values a deployment would give `Example`, as a document written to a temp file.
    fn populated_file(name: &str, without: &[&str], capabilities: &str) -> PathBuf {
        let mut doc = scaffold_document::<Example>();
        let values: HashMap<&str, &str> = CASSINI_ENV
            .iter()
            .chain(GRAPH_ENV.iter())
            .copied()
            .chain([
                ("EXAMPLE_BUCKET", "the-bucket"),
                ("POLAR_CAPABILITIES", capabilities),
            ])
            .collect();
        for variable in doc
            .document
            .sections
            .iter_mut()
            .flat_map(|s| s.variables.iter_mut())
        {
            if without.contains(&variable.name.as_str()) {
                continue;
            }
            if variable.secret {
                variable.value = Some(DocValue::Reference {
                    env: format!("SECRET_FOR_{}", variable.name),
                });
            } else if let Some(value) = values.get(variable.name.as_str()) {
                variable.value = Some(DocValue::Literal(value.to_string()));
            }
        }
        let dir = std::env::temp_dir().join(format!("polar-agent-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        path
    }

    /// The process environment of a pod whose secrets are wired.
    fn with_secrets() -> impl Env + 'static {
        |name: &str| {
            name.strip_prefix("SECRET_FOR_")
                .map(|_| "hunter2".to_string())
        }
    }

    fn variable<'a>(doc: &'a AgentDocument, name: &str) -> &'a polar_env::DocVariable {
        doc.document
            .sections
            .iter()
            .flat_map(|s| s.variables.iter())
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("{name} is not in the document"))
    }

    // -- a service that is not a Cassini client ----------------------------------------------

    #[test]
    fn a_service_is_not_given_the_cassini_client() {
        assert!(needs_of::<Serving>().is_empty());
        assert_eq!(
            needs_of::<Example>()[0],
            Capability::Cassini,
            "an agent still is"
        );
        let document = scaffold_document::<Serving>();
        assert!(document.requires.is_empty());
        let sections: Vec<&str> = document
            .document
            .sections
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            !sections.contains(&"cassini-client"),
            "no client variables are asked of a service: {sections:?}"
        );
        assert!(sections.contains(&"health") && sections.contains(&"builder"));
    }

    #[test]
    fn a_service_that_contradicts_itself_is_told_so() {
        let problems = declaration_problems::<Muddled>();
        assert!(
            problems.iter().any(|p| p.contains("SELF_TEST")),
            "a service has nothing to test itself against: {problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("Cassini")),
            "it lists the Cassini client it says it is not: {problems:?}"
        );
        assert!(declaration_problems::<Serving>().is_empty());
    }

    #[test]
    fn the_help_of_a_service_does_not_say_it_connects_to_cassini() {
        let text = help_text::<Serving>();
        assert!(!text.contains("connects to the Cassini broker"), "{text}");
        assert!(text.contains("not a Cassini client"), "{text}");
    }

    // -- the command line --------------------------------------------------------------------

    #[test]
    fn no_arguments_run_the_agent() {
        assert_eq!(parse_command(&[]), Ok(Command::Run));
    }

    #[test]
    fn the_flags_are_understood() {
        assert_eq!(parse_command(&strings(&["--help"])), Ok(Command::Help));
        assert_eq!(
            parse_command(&strings(&["--scaffold"])),
            Ok(Command::Scaffold { config: None })
        );
        assert_eq!(
            parse_command(&strings(&["--scaffold", "--config", "f.json"])),
            Ok(Command::Scaffold {
                config: Some(PathBuf::from("f.json"))
            })
        );
        assert_eq!(
            parse_command(&strings(&["--check", "--config", "f.json"])),
            Ok(Command::Check {
                config: PathBuf::from("f.json")
            })
        );
    }

    #[test]
    fn a_check_without_a_document_and_an_unknown_flag_are_refused_with_the_usage() {
        let missing = parse_command(&strings(&["--check"])).unwrap_err();
        assert!(missing.contains("--config"), "{missing}");
        let unknown = parse_command(&strings(&["--describe"])).unwrap_err();
        assert!(unknown.contains("usage:"), "{unknown}");
        let dangling = parse_command(&strings(&["--scaffold", "--config"])).unwrap_err();
        assert!(dangling.contains("--config"), "{dangling}");
    }

    // -- help ----------------------------------------------------------------------------------

    #[test]
    fn help_says_what_the_agent_is_what_it_needs_and_what_it_reads() {
        let text = help_text::<Example>();
        for expected in [
            "example-agent",
            "Copies things from one place to another",
            "It reads from the bucket and writes the graph.",
            "cassini",
            "graph-write",
            "EXAMPLE_BUCKET",
            "30",
            "--scaffold",
            "--check",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
        assert!(!text.contains("EXAMPLE_TOKEN="), "no values in help");
    }

    #[test]
    fn an_agent_without_long_help_still_gets_a_help() {
        let text = help_text::<Bare>();
        assert!(
            text.contains("bare-agent") && text.contains("Needs nothing"),
            "{text}"
        );
        assert!(text.contains("cassini"), "the client is implied: {text}");
    }

    // -- the scaffold --------------------------------------------------------------------------

    #[test]
    fn the_scaffold_names_the_agent_and_what_it_requires() {
        let doc = scaffold_document::<Example>();
        assert_eq!(doc.agent.name, "example-agent");
        assert_eq!(doc.requires, vec!["cassini", "graph-write"]);
        assert_eq!(doc.shutdown_budget_minimum_secs, 30);
        assert_eq!(doc.document.format, polar_env::DOCUMENT_FORMAT);
    }

    #[test]
    fn the_scaffold_holds_every_topic_the_agent_uses_and_where_each_comes_from() {
        use crate::topics::{Access, Origin, Rule};
        let doc = scaffold_document::<Example>();
        let find = |name: &str, access: Access| {
            doc.topics
                .iter()
                .find(|t| t.name == name && t.access == access)
                .unwrap_or_else(|| panic!("{name} is not in {:?}", doc.topics))
                .origin
                .clone()
        };
        assert_eq!(
            find("bucket.copied", Access::Publish),
            Origin::Declared {
                purpose: "what it copied".into(),
                waits: None,
            }
        );
        assert!(matches!(
            find("agent.example-agent.configuration", Access::Subscribe),
            Origin::Implied {
                rule: Rule::Agent,
                ..
            }
        ));
        assert!(matches!(
            find("selftest.example-agent", Access::Publish),
            Origin::Implied {
                rule: Rule::SelfTest,
                ..
            }
        ));
        // It is the set the builder computes, whole.
        assert_eq!(doc.topics, topic_set::<Example>().entries());
    }

    #[test]
    fn a_family_is_declared_in_the_scaffold_and_read_from_its_own_variable() {
        let doc = scaffold_document::<Scheduling>();
        assert_eq!(doc.topic_families.len(), 1);
        assert_eq!(
            doc.topic_families[0].pattern,
            "agent.{workload}.configuration"
        );
        assert_eq!(doc.topic_families[0].over, "POLAR_SERVED_WORKLOADS");
        // Filled from nothing, it serves no workload, so nothing is expanded.
        assert!(
            doc.topics
                .iter()
                .all(|t| t.access != crate::topics::Access::Publish
                    || !t.name.ends_with(".configuration"))
        );
        let variables = |doc: &AgentDocument| -> Vec<String> {
            doc.document
                .sections
                .iter()
                .flat_map(|s| s.variables.iter().map(|v| v.name.clone()))
                .collect()
        };
        assert!(variables(&doc).contains(&"POLAR_SERVED_WORKLOADS".to_string()));
        // An agent with no family does not read it.
        assert!(
            !variables(&scaffold_document::<Example>())
                .contains(&"POLAR_SERVED_WORKLOADS".to_string())
        );
        assert!(declaration_problems::<Scheduling>().is_empty());
    }

    #[test]
    fn filled_from_a_document_the_scaffold_expands_a_family_for_each_workload_served() {
        let mut doc = scaffold_document::<Scheduling>();
        let values: HashMap<&str, &str> = CASSINI_ENV
            .iter()
            .copied()
            .chain([("POLAR_SERVED_WORKLOADS", "kube-observer,jira-observer")])
            .collect();
        for variable in doc
            .document
            .sections
            .iter_mut()
            .flat_map(|s| s.variables.iter_mut())
        {
            if let Some(value) = values.get(variable.name.as_str()) {
                variable.value = Some(DocValue::Literal(value.to_string()));
            }
        }
        let dir = std::env::temp_dir().join(format!("polar-agent-{}-families", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        let filled: AgentDocument = serde_json::from_str(
            &scaffold_json::<Scheduling>(Some(&path), |_: &str| None).unwrap(),
        )
        .unwrap();
        let expanded: Vec<(String, String)> = filled
            .topics
            .iter()
            .filter_map(|t| match &t.origin {
                crate::topics::Origin::Family { workload, .. } => {
                    Some((t.name.clone(), workload.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            expanded,
            [
                (
                    "agent.jira-observer.configuration".to_string(),
                    "jira-observer".to_string()
                ),
                (
                    "agent.kube-observer.configuration".to_string(),
                    "kube-observer".to_string()
                ),
            ]
        );
    }

    #[test]
    fn asked_with_no_document_the_scaffold_expands_a_family_for_the_workloads_its_environment_names()
     {
        let env =
            |name: &str| (name == "POLAR_SERVED_WORKLOADS").then(|| "b-agent,a-agent".to_string());
        let doc: AgentDocument =
            serde_json::from_str(&scaffold_json::<Scheduling>(None, env).unwrap()).unwrap();
        let expanded: Vec<String> = doc
            .topics
            .iter()
            .filter(|t| matches!(t.origin, crate::topics::Origin::Family { .. }))
            .map(|t| t.name.clone())
            .collect();
        assert_eq!(
            expanded,
            ["agent.a-agent.configuration", "agent.b-agent.configuration"]
        );
        // With nothing named, nothing is expanded.
        let bare: AgentDocument =
            serde_json::from_str(&scaffold_json::<Scheduling>(None, |_: &str| None).unwrap())
                .unwrap();
        assert!(
            bare.topics
                .iter()
                .all(|t| !matches!(t.origin, crate::topics::Origin::Family { .. }))
        );
    }

    #[test]
    fn a_service_that_is_not_a_cassini_client_uses_no_topics() {
        assert!(scaffold_document::<Serving>().topics.is_empty());
        assert!(scaffold_document::<Serving>().groups.is_empty());
        assert_eq!(
            scaffold_document::<Example>().groups,
            ["cassini-client", "self-test"]
        );
    }

    #[test]
    fn declaring_an_implied_topic_is_a_declaration_problem() {
        let said = declaration_problems::<Redundant>().join("\n");
        assert!(said.contains("agent.redundant.configuration"), "{said}");
        assert!(declaration_problems::<Example>().is_empty());
    }

    #[test]
    fn the_scaffold_holds_the_sections_that_apply_tagged_with_what_brought_them() {
        let doc = scaffold_document::<Example>();
        let tags: Vec<(String, Option<String>)> = doc
            .document
            .sections
            .iter()
            .map(|s| (s.name.clone(), s.capability.clone()))
            .collect();
        for expected in [
            ("builder", None),
            ("health", None),
            ("example", None),
            ("cassini-client", Some("cassini")),
            ("graph", Some("graph-write")),
        ] {
            assert!(
                tags.contains(&(expected.0.to_string(), expected.1.map(str::to_string))),
                "{expected:?} not in {tags:?}"
            );
        }
        assert!(
            !tags.iter().any(|(name, _)| name == "crypto"),
            "a capability it does not need brings nothing: {tags:?}"
        );
    }

    #[test]
    fn a_bare_agents_scaffold_has_only_the_sections_every_agent_has_and_the_clients() {
        let doc = scaffold_document::<Bare>();
        let names: Vec<_> = doc
            .document
            .sections
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        // Every agent is a Cassini client, so it is implied: an agent cannot forget it. And every
        // agent logs and traces as its deployment says.
        assert_eq!(
            names,
            vec!["builder", "health", "logging", "cassini-client"]
        );
        assert_eq!(doc.requires, vec!["cassini"]);
    }

    #[test]
    fn the_scaffold_has_no_values_and_marks_the_secrets() {
        let doc = scaffold_document::<Example>();
        assert!(
            doc.document
                .sections
                .iter()
                .flat_map(|s| s.variables.iter())
                .all(|v| v.value.is_none())
        );
        assert!(variable(&doc, "EXAMPLE_TOKEN").secret);
        assert!(variable(&doc, "GRAPH_PASSWORD").secret);
        assert!(variable(&doc, "BROKER_ADDR").required);
    }

    #[test]
    fn the_scaffold_as_json_reads_back_as_the_same_document() {
        let text = scaffold_json::<Example>(None, |_: &str| None::<String>).unwrap();
        let back: AgentDocument = serde_json::from_str(&text).unwrap();
        assert_eq!(back, scaffold_document::<Example>());
    }

    // -- filling it from a file ----------------------------------------------------------------

    #[test]
    fn a_complete_document_scaffolds_filled_with_sources_and_resolved_values_and_no_problems() {
        let path = populated_file("complete", &[], "cassini,graph-write");
        let text = scaffold_json::<Example>(Some(&path), with_secrets()).unwrap();
        let doc: AgentDocument = serde_json::from_str(&text).unwrap();
        assert_eq!(doc.problems, Vec::<String>::new(), "{text}");

        let bucket = variable(&doc, "EXAMPLE_BUCKET");
        assert_eq!(bucket.value, Some(DocValue::Literal("the-bucket".into())));
        assert_eq!(bucket.source.as_deref(), Some("file"));
        assert_eq!(bucket.resolved.as_deref(), Some("the-bucket"));

        let token = variable(&doc, "EXAMPLE_TOKEN");
        assert!(matches!(token.value, Some(DocValue::Reference { .. })));
        assert_eq!(
            token.resolved.as_deref(),
            Some("<hidden>"),
            "a secret is never shown"
        );
        assert!(!text.contains("hunter2"));

        let tick = variable(&doc, "POLAR_HEALTH_TICK_SECS");
        assert_eq!(tick.source.as_deref(), Some("default"));
        assert_eq!(tick.resolved.as_deref(), Some("30"));

        let queue = variable(&doc, "CASSINI_QUEUE_PATH");
        assert_eq!(queue.source.as_deref(), Some("unset"));
    }

    #[test]
    fn an_incomplete_document_scaffolds_with_the_problem_named() {
        let path = populated_file("incomplete", &["EXAMPLE_BUCKET"], "cassini,graph-write");
        let text = scaffold_json::<Example>(Some(&path), with_secrets()).unwrap();
        let doc: AgentDocument = serde_json::from_str(&text).unwrap();
        assert!(
            doc.problems.iter().any(|p| p.contains("EXAMPLE_BUCKET")),
            "{:?}",
            doc.problems
        );
    }

    #[test]
    fn a_file_that_is_not_a_document_is_an_error_not_a_scaffold() {
        let error = scaffold_json::<Example>(Some(Path::new("/no/such/config.json")), |_: &str| {
            None::<String>
        })
        .unwrap_err();
        assert!(error.contains("/no/such/config.json"), "{error}");
    }

    // -- check ---------------------------------------------------------------------------------

    #[test]
    fn a_complete_document_passes_the_check() {
        let path = populated_file("check-ok", &[], "cassini,graph-write");
        assert_eq!(
            check::<Example>(&path, with_secrets()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_missing_required_value_fails_the_check_naming_it() {
        let path = populated_file("check-missing", &["EXAMPLE_BUCKET"], "cassini,graph-write");
        let problems = check::<Example>(&path, with_secrets()).join(" | ");
        assert!(problems.contains("EXAMPLE_BUCKET"), "{problems}");
    }

    #[test]
    fn a_secret_whose_environment_variable_is_not_set_fails_the_check() {
        let path = populated_file("check-secret", &[], "cassini,graph-write");
        let problems = check::<Example>(&path, |_: &str| None::<String>).join(" | ");
        assert!(problems.contains("EXAMPLE_TOKEN"), "{problems}");
        assert!(
            !problems.contains("SECRET_FOR_"),
            "the variable's value, not its name: {problems}"
        );
    }

    #[test]
    fn a_deployment_that_does_not_provide_what_the_code_needs_fails_the_check() {
        let path = populated_file("check-caps", &[], "cassini");
        let problems = check::<Example>(&path, with_secrets()).join(" | ");
        assert!(problems.contains("graph-write"), "{problems}");
    }

    /// `populated_file`, with one more variable given a literal value afterwards.
    fn populated_file_with(name: &str, variable_name: &str, value: &str) -> PathBuf {
        let path = populated_file(name, &[], "cassini,graph-write");
        let mut doc: AgentDocument =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for variable in doc
            .document
            .sections
            .iter_mut()
            .flat_map(|s| s.variables.iter_mut())
        {
            if variable.name == variable_name {
                variable.value = Some(DocValue::Literal(value.into()));
            }
        }
        std::fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();
        path
    }

    #[test]
    fn a_budget_below_the_least_the_code_needs_fails_the_check() {
        let path = populated_file_with("budget", "POLAR_SHUTDOWN_BUDGET_SECS", "10");
        let problems = check::<Example>(&path, with_secrets()).join(" | ");
        assert!(problems.contains("30"), "{problems}");
    }

    #[test]
    fn a_secret_written_as_a_literal_fails_the_check_without_repeating_it() {
        let path = populated_file_with("literal", "EXAMPLE_TOKEN", "hunter2");
        let problems = check::<Example>(&path, with_secrets()).join(" | ");
        assert!(problems.contains("EXAMPLE_TOKEN"), "{problems}");
        assert!(!problems.contains("hunter2"), "{problems}");
    }

    #[test]
    fn a_value_for_a_variable_the_agent_does_not_declare_fails_the_check() {
        // A document written for another agent, or by hand: the extra variable is refused.
        let path = populated_file("undeclared", &[], "cassini,graph-write");
        let mut doc: AgentDocument =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc.document.sections[0]
            .variables
            .push(polar_env::DocVariable {
                name: "SOMETHING_ELSE".into(),
                type_name: "text".into(),
                required: false,
                secret: false,
                default: None,
                mutability: "static".into(),
                doc: "not this agent's".into(),
                value: Some(DocValue::Literal("x".into())),
                source: None,
                resolved: None,
            });
        std::fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();
        let problems = check::<Example>(&path, with_secrets()).join(" | ");
        assert!(problems.contains("SOMETHING_ELSE"), "{problems}");
        assert!(problems.contains("example-agent"), "{problems}");
    }

    // -- running -------------------------------------------------------------------------------

    #[test]
    fn the_flags_are_answered_without_starting_the_agent() {
        WATCHED_STARTED.store(false, Ordering::SeqCst);
        // A document with every value a Watched agent needs: the Cassini client's.
        let mut doc = scaffold_document::<Watched>();
        for variable in doc
            .document
            .sections
            .iter_mut()
            .flat_map(|s| s.variables.iter_mut())
        {
            if let Some((_, value)) = CASSINI_ENV.iter().find(|(name, _)| *name == variable.name) {
                variable.value = Some(DocValue::Literal(value.to_string()));
            }
        }
        let dir = std::env::temp_dir().join(format!("polar-agent-{}-watched", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, serde_json::to_string(&doc).unwrap()).unwrap();
        let config = path.display().to_string();

        assert_eq!(
            run_with::<Watched>(&strings(&["--help"]), with_secrets()),
            0
        );
        assert_eq!(
            run_with::<Watched>(&strings(&["--scaffold"]), with_secrets()),
            0
        );
        assert_eq!(
            run_with::<Watched>(&strings(&["--check", "--config", &config]), with_secrets()),
            0
        );
        assert_eq!(
            run_with::<Watched>(&strings(&["--nonsense"]), with_secrets()),
            2
        );
        assert!(!WATCHED_STARTED.load(Ordering::SeqCst));
    }

    #[test]
    fn a_check_of_a_bad_document_exits_one() {
        let path = populated_file("flags-bad", &["EXAMPLE_BUCKET"], "cassini,graph-write");
        let config = path.display().to_string();
        assert_eq!(
            run_with::<Example>(&strings(&["--check", "--config", &config]), with_secrets()),
            1
        );
    }

    #[test]
    fn an_agent_is_started_with_the_configuration_its_document_gave_and_exits_nonzero_when_it_ends()
    {
        STARTED.store(false, Ordering::SeqCst);
        let path = populated_file("start", &[], "cassini,graph-write");
        let config = path.display().to_string();
        let process = move |name: &str| {
            if name == "POLAR_CONFIG" {
                Some(config.clone())
            } else {
                name.strip_prefix("SECRET_FOR_")
                    .map(|_| "hunter2".to_string())
            }
        };
        let status = run_with::<Example>(&[], process);
        assert!(
            STARTED.load(Ordering::SeqCst),
            "start was called, with its own setting readable"
        );
        assert_eq!(
            status, 1,
            "an agent that ends has failed, even when it returned Ok"
        );
    }

    #[test]
    fn an_agent_whose_configuration_is_not_sound_is_not_started() {
        WATCHED_STARTED.store(false, Ordering::SeqCst);
        let status = run_with::<Watched>(&[], |_: &str| None::<String>);
        assert_eq!(status, 1);
        assert!(!WATCHED_STARTED.load(Ordering::SeqCst));
    }

    // -- a Cassini client the agent's health does not count ------------------------------------

    static CLIENT_RAN: AtomicBool = AtomicBool::new(false);
    static CLIENT_EXPECTS_CASSINI: AtomicBool = AtomicBool::new(false);

    /// A Cassini client with no dependency on Cassini, as cert-issuer is (docs/architecture/cert-issuer-isolation.md,
    /// "The model", revised 2026-10-23).
    struct Independent;

    impl PolarAgent for Independent {
        const NAME: &'static str = "independent";
        const PURPOSE: &'static str = "Speaks over Cassini without depending on it";
        const NEEDS: &'static [Capability] = &[Capability::Cassini];
        const DEPENDS_ON_CASSINI: bool = false;
        const SELF_TEST: bool = false;
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
        async fn start(configured: Configured) -> Result<(), StartError> {
            CLIENT_EXPECTS_CASSINI
                .store(configured.startup.health.expects_cassini, Ordering::SeqCst);
            CLIENT_RAN.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// The same client, saying nothing of its dependency.
    struct Dependent;

    impl PolarAgent for Dependent {
        const NAME: &'static str = "dependent";
        const PURPOSE: &'static str = "Speaks over Cassini and depends on it";
        const NEEDS: &'static [Capability] = &[Capability::Cassini];
        const SELF_TEST: bool = false;
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
        async fn start(configured: Configured) -> Result<(), StartError> {
            CLIENT_EXPECTS_CASSINI
                .store(configured.startup.health.expects_cassini, Ordering::SeqCst);
            CLIENT_RAN.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn the_scaffold_says_when_an_agent_does_not_depend_on_cassini_and_says_nothing_otherwise() {
        let independent = scaffold_document::<Independent>();
        assert!(!independent.depends_on_cassini);
        let text = serde_json::to_string(&independent).unwrap();
        assert!(text.contains("\"depends_on_cassini\":false"), "{text}");

        // Every other agent's scaffold is as it was, and one written before reads as depending on Cassini.
        let dependent = scaffold_document::<Dependent>();
        assert!(dependent.depends_on_cassini);
        let text = serde_json::to_string(&dependent).unwrap();
        assert!(!text.contains("depends_on_cassini"), "{text}");
        let read: AgentDocument = serde_json::from_str(&text).unwrap();
        assert!(read.depends_on_cassini);
        let independent_read: AgentDocument =
            serde_json::from_str(&serde_json::to_string(&independent).unwrap()).unwrap();
        assert!(!independent_read.depends_on_cassini);
    }

    #[test]
    fn an_agent_that_does_not_depend_on_cassini_is_started_with_a_health_that_does_not_count_it() {
        CLIENT_RAN.store(false, Ordering::SeqCst);
        CLIENT_EXPECTS_CASSINI.store(true, Ordering::SeqCst);
        assert_eq!(
            run_with::<Independent>(&[], either_process(Some("cassini"), true)),
            1
        );
        assert!(CLIENT_RAN.load(Ordering::SeqCst), "it started");
        assert!(
            !CLIENT_EXPECTS_CASSINI.load(Ordering::SeqCst),
            "its health does not count its session"
        );

        CLIENT_RAN.store(false, Ordering::SeqCst);
        assert_eq!(
            run_with::<Dependent>(&[], either_process(Some("cassini"), true)),
            1
        );
        assert!(CLIENT_RAN.load(Ordering::SeqCst), "it started");
        assert!(
            CLIENT_EXPECTS_CASSINI.load(Ordering::SeqCst),
            "an agent that says nothing has a health that counts its session"
        );
    }

    // -- an optional Cassini client ----------------------------------------------------------

    static EITHER_RAN: AtomicBool = AtomicBool::new(false);
    static EITHER_CASSINI: AtomicBool = AtomicBool::new(false);

    /// A service that runs on its own or as part of a cluster.
    struct Either;

    impl PolarAgent for Either {
        const NAME: &'static str = "either";
        const PURPOSE: &'static str = "Runs alone or in a cluster";
        const NEEDS: &'static [Capability] = &[];
        const OPTIONAL: &'static [Capability] = &[Capability::Cassini];
        const CASSINI_CLIENT: bool = false;
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
        async fn start(configured: Configured) -> Result<(), StartError> {
            EITHER_CASSINI.store(configured.cassini_enabled(), Ordering::SeqCst);
            EITHER_RAN.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// The Cassini variables and the capabilities a deployment gives `Either`, as the process
    /// environment of a run with no document.
    fn either_process(capabilities: Option<&'static str>, cassini: bool) -> impl Env + 'static {
        move |name: &str| {
            if name == "POLAR_CAPABILITIES" {
                return capabilities.map(str::to_string);
            }
            if cassini {
                CASSINI_ENV
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            } else {
                None
            }
        }
    }

    /// A document for `Either`: the Cassini values if asked, the capabilities if given.
    fn either_file(name: &str, cassini: bool, capabilities: Option<&str>) -> String {
        let mut doc = scaffold_document::<Either>();
        for variable in doc
            .document
            .sections
            .iter_mut()
            .flat_map(|s| s.variables.iter_mut())
        {
            if variable.name == "POLAR_CAPABILITIES" {
                variable.value = capabilities.map(|c| DocValue::Literal(c.to_string()));
            } else if cassini {
                if variable.secret {
                    variable.value = Some(DocValue::Reference {
                        env: format!("SECRET_FOR_{}", variable.name),
                    });
                } else if let Some((_, value)) =
                    CASSINI_ENV.iter().find(|(key, _)| *key == variable.name)
                {
                    variable.value = Some(DocValue::Literal(value.to_string()));
                }
            }
        }
        let dir = std::env::temp_dir().join(format!("polar-agent-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        path.display().to_string()
    }

    #[test]
    fn an_optional_capability_is_used_only_when_the_deployment_provides_it() {
        assert!(
            effective_of::<Either>(None).is_empty(),
            "no deployment, no client"
        );
        assert!(effective_of::<Either>(Some("")).is_empty());
        assert!(effective_of::<Either>(Some("graph-write,tracing")).is_empty());
        assert_eq!(
            effective_of::<Either>(Some("tracing, cassini")),
            vec![Capability::Cassini]
        );
        // An agent that is always a client is one whatever the deployment says.
        assert_eq!(
            effective_of::<Example>(None)[0],
            Capability::Cassini,
            "an agent still is"
        );
        // A service with nothing optional does not become a client because the deployment has one.
        assert_eq!(effective_of::<Serving>(Some("cassini")), Vec::new());
    }

    #[test]
    fn an_agent_with_an_optional_cassini_client_declares_itself_soundly() {
        assert_eq!(declaration_problems::<Either>(), Vec::<String>::new());
    }

    struct OptionalGraph;

    impl PolarAgent for OptionalGraph {
        const NAME: &'static str = "optional-graph";
        const PURPOSE: &'static str = "Tries to make the graph optional";
        const NEEDS: &'static [Capability] = &[];
        const OPTIONAL: &'static [Capability] = &[Capability::GraphWrite];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    struct AlwaysAndOptional;

    impl PolarAgent for AlwaysAndOptional {
        const NAME: &'static str = "always-and-optional";
        const PURPOSE: &'static str = "Is a client and may do without";
        const NEEDS: &'static [Capability] = &[];
        const OPTIONAL: &'static [Capability] = &[Capability::Cassini];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    struct NeededAndOptional;

    impl PolarAgent for NeededAndOptional {
        const NAME: &'static str = "needed-and-optional";
        const PURPOSE: &'static str = "Needs it and may do without";
        const NEEDS: &'static [Capability] = &[Capability::Cassini, Capability::GraphWrite];
        const OPTIONAL: &'static [Capability] = &[Capability::GraphWrite, Capability::GraphWrite];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    #[test]
    fn contradictory_optional_declarations_are_told_so() {
        let problems = declaration_problems::<OptionalGraph>();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("graph-write cannot be optional")),
            "{problems:?}"
        );
        let problems = declaration_problems::<AlwaysAndOptional>();
        assert!(
            problems.iter().any(|p| p.contains("CASSINI_CLIENT")),
            "an agent that is always a client cannot do without: {problems:?}"
        );
        let problems = declaration_problems::<NeededAndOptional>();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("both in NEEDS and in OPTIONAL")),
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("more than once in OPTIONAL")),
            "{problems:?}"
        );
    }

    #[test]
    fn the_scaffold_lists_what_is_optional_and_asks_for_its_variables_without_demanding_them() {
        let document = scaffold_document::<Either>();
        assert!(document.requires.is_empty());
        assert_eq!(document.optional, vec!["cassini".to_string()]);
        let client = document
            .document
            .sections
            .iter()
            .find(|s| s.name == "cassini-client")
            .expect("the optional capability's section is in the scaffold");
        assert_eq!(client.capability.as_deref(), Some("cassini"));
        // An agent without optional capabilities exports exactly what it did before.
        let json = serde_json::to_value(scaffold_document::<Example>()).unwrap();
        assert!(json.get("optional").is_none(), "no empty list is written");
    }

    #[test]
    fn the_help_says_it_runs_on_its_own_or_in_a_cluster() {
        let text = help_text::<Either>();
        assert!(
            text.contains("runs on its own or as part of a cluster"),
            "{text}"
        );
        assert!(
            text.contains("when its deployment provides them: cassini"),
            "{text}"
        );
    }

    #[test]
    fn a_configuration_without_cassini_is_sound_when_the_deployment_does_not_provide_it() {
        for (label, capabilities) in [
            ("unset", None),
            ("empty", Some("")),
            ("other", Some("tracing")),
        ] {
            let config = either_file(&format!("alone-{label}"), false, capabilities);
            assert_eq!(
                check::<Either>(Path::new(&config), with_secrets()),
                Vec::<String>::new(),
                "{label}"
            );
        }
    }

    #[test]
    fn a_deployment_that_provides_cassini_must_configure_it() {
        let missing = either_file("cluster-missing", false, Some("cassini"));
        let problems = check::<Either>(Path::new(&missing), with_secrets());
        assert!(
            problems.iter().any(|p| p.contains("BROKER_ADDR")),
            "the client's settings are demanded once Cassini is provided: {problems:?}"
        );
        let whole = either_file("cluster-whole", true, Some("cassini"));
        assert_eq!(
            check::<Either>(Path::new(&whole), with_secrets()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_same_service_starts_alone_and_in_a_cluster() {
        // On its own: nothing in the environment.
        EITHER_RAN.store(false, Ordering::SeqCst);
        EITHER_CASSINI.store(true, Ordering::SeqCst);
        assert_eq!(run_with::<Either>(&[], either_process(None, false)), 1);
        assert!(
            EITHER_RAN.load(Ordering::SeqCst),
            "it started with no cluster"
        );
        assert!(
            !EITHER_CASSINI.load(Ordering::SeqCst),
            "and without a Cassini client"
        );

        // In a cluster: the deployment provides Cassini and configures it.
        EITHER_RAN.store(false, Ordering::SeqCst);
        assert_eq!(
            run_with::<Either>(&[], either_process(Some("cassini"), true)),
            1
        );
        assert!(EITHER_RAN.load(Ordering::SeqCst));
        assert!(
            EITHER_CASSINI.load(Ordering::SeqCst),
            "it was told to be a client"
        );

        // Provided but not configured: refused, as an agent's missing settings always are.
        EITHER_RAN.store(false, Ordering::SeqCst);
        assert_eq!(
            run_with::<Either>(&[], either_process(Some("cassini"), false)),
            1
        );
        assert!(
            !EITHER_RAN.load(Ordering::SeqCst),
            "it did not start half-configured"
        );
    }

    // -- commands ---------------------------------------------------------------------------

    const BACKUP_ARGS: &[crate::command::ArgSpec] = &[crate::command::ArgSpec {
        name: "destination",
        kind: crate::command::ArgKind::Text,
        required: true,
        doc: "where the backup goes",
    }];

    const BACKUP: CommandSpec = CommandSpec {
        name: "backup-now",
        doc: "Back up the data now",
        args: BACKUP_ARGS,
        delivery: crate::command::Delivery::Once,
        exclusive: true,
        timeout: Duration::from_secs(900),
    };

    struct Commanding;
    impl PolarAgent for Commanding {
        const NAME: &'static str = "commanding";
        const PURPOSE: &'static str = "Declares one command";
        const NEEDS: &'static [Capability] = &[Capability::Cassini];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);
        const COMMANDS: &'static [CommandSpec] = &[BACKUP];
        async fn start(_: Configured) -> Result<(), StartError> {
            COMMANDING_STARTED.store(true, Ordering::SeqCst);
            Ok(())
        }
    }
    static COMMANDING_STARTED: AtomicBool = AtomicBool::new(false);

    #[test]
    fn the_schema_export_lists_the_commands_an_agent_declares() {
        let doc = scaffold_document::<Commanding>();
        assert_eq!(doc.commands.len(), 1);
        let command = &doc.commands[0];
        assert_eq!(command.name, "backup-now");
        assert_eq!(command.delivery, "once");
        assert!(command.exclusive);
        assert_eq!(command.timeout_secs, 900);
        assert_eq!(command.args.len(), 1);
        assert_eq!(command.args[0].name, "destination");
        assert_eq!(command.args[0].kind, "text");
        assert!(command.args[0].required);

        // As the JSON a tool reads, and read back.
        let json = serde_json::to_value(&doc).unwrap();
        assert_eq!(json["commands"][0]["name"], "backup-now");
        assert_eq!(json["commands"][0]["args"][0]["kind"], "text");
        let back: AgentDocument = serde_json::from_value(json).unwrap();
        assert_eq!(back, doc);

        // An agent with none says so, and an older document without the section still reads.
        let none = serde_json::to_value(scaffold_document::<Example>()).unwrap();
        assert_eq!(none["commands"], serde_json::json!([]));
        let mut older = none.clone();
        older.as_object_mut().unwrap().remove("commands");
        let read: AgentDocument = serde_json::from_value(older).unwrap();
        assert!(read.commands.is_empty());
    }

    #[test]
    fn a_sound_command_declaration_has_no_problems() {
        assert!(declaration_problems::<Commanding>().is_empty());
    }

    struct BadCommands;
    impl PolarAgent for BadCommands {
        const NAME: &'static str = "bad-commands";
        const PURPOSE: &'static str = "Declares commands badly";
        const NEEDS: &'static [Capability] = &[Capability::Cassini];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);
        const COMMANDS: &'static [CommandSpec] = &[BACKUP, BACKUP];
        async fn start(_: Configured) -> Result<(), StartError> {
            BAD_COMMANDS_STARTED.store(true, Ordering::SeqCst);
            Ok(())
        }
    }
    static BAD_COMMANDS_STARTED: AtomicBool = AtomicBool::new(false);

    #[test]
    fn an_agent_that_declares_its_commands_badly_says_so_and_is_not_started() {
        let problems = declaration_problems::<BadCommands>();
        assert!(
            problems.iter().any(|p| p.contains("declared twice")),
            "{problems:?}"
        );
        BAD_COMMANDS_STARTED.store(false, Ordering::SeqCst);
        let status = run_with::<BadCommands>(&[], with_secrets());
        assert_eq!(status, 1);
        assert!(!BAD_COMMANDS_STARTED.load(Ordering::SeqCst));
    }

    struct ServiceWithCommands;
    impl PolarAgent for ServiceWithCommands {
        const NAME: &'static str = "service-with-commands";
        const PURPOSE: &'static str = "Is not a client";
        const NEEDS: &'static [Capability] = &[];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);
        const SELF_TEST: bool = false;
        const CASSINI_CLIENT: bool = false;
        const COMMANDS: &'static [CommandSpec] = &[BACKUP];
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    #[test]
    fn commands_cannot_be_declared_by_a_service_that_nothing_can_reach() {
        let problems = declaration_problems::<ServiceWithCommands>();
        assert!(
            problems.iter().any(|p| p.contains("not a Cassini client")),
            "{problems:?}"
        );
    }

    // -- runtime sections ------------------------------------------------------------------

    static REPOS: crate::runtime::RuntimeSection = crate::runtime::RuntimeSection {
        authority: crate::runtime::SectionAuthority::Runtime,
        name: "repositories",
        doc: "The repositories to observe.",
        body: r#"{"repositories": {"list_of": {"url": {"type": "https URL", "required": true}}}}"#,
        validate: |_| Ok(()),
    };

    static RUNTIME_ONE: [&crate::runtime::RuntimeSection; 1] = [&REPOS];
    static RUNTIME_TWICE: [&crate::runtime::RuntimeSection; 2] = [&REPOS, &REPOS];

    struct WithRuntime;
    impl PolarAgent for WithRuntime {
        const NAME: &'static str = "with-runtime";
        const PURPOSE: &'static str = "Declares one runtime section";
        const NEEDS: &'static [Capability] = &[Capability::Cassini];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);
        const RUNTIME: &'static [&'static crate::runtime::RuntimeSection] = &RUNTIME_ONE;
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    #[test]
    fn the_schema_export_lists_the_runtime_sections_an_agent_declares() {
        let doc = scaffold_document::<WithRuntime>();
        assert_eq!(doc.runtime.len(), 1);
        assert_eq!(doc.runtime[0].name, "repositories");
        assert_eq!(doc.runtime[0].doc, "The repositories to observe.");
        assert_eq!(
            doc.runtime[0].body["repositories"]["list_of"]["url"]["required"],
            true
        );

        // Next to the variable sections, in the JSON a tool reads, and read back.
        let json = serde_json::to_value(&doc).unwrap();
        assert!(json["sections"].is_array());
        assert_eq!(json["runtime"][0]["name"], "repositories");
        let back: AgentDocument = serde_json::from_value(json).unwrap();
        assert_eq!(back, doc);

        // An agent with none says so, and an older document without the list still reads.
        let none = serde_json::to_value(scaffold_document::<Example>()).unwrap();
        assert_eq!(none["runtime"], serde_json::json!([]));
        let mut older = none.clone();
        older.as_object_mut().unwrap().remove("runtime");
        let read: AgentDocument = serde_json::from_value(older).unwrap();
        assert!(read.runtime.is_empty());
    }

    struct TwiceDeclared;
    impl PolarAgent for TwiceDeclared {
        const NAME: &'static str = "twice-declared";
        const PURPOSE: &'static str = "Declares a section twice";
        const NEEDS: &'static [Capability] = &[Capability::Cassini];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);
        const RUNTIME: &'static [&'static crate::runtime::RuntimeSection] = &RUNTIME_TWICE;
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    struct ServiceWithRuntime;
    impl PolarAgent for ServiceWithRuntime {
        const NAME: &'static str = "service-with-runtime";
        const PURPOSE: &'static str = "Is not a client";
        const NEEDS: &'static [Capability] = &[];
        const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);
        const SELF_TEST: bool = false;
        const CASSINI_CLIENT: bool = false;
        const RUNTIME: &'static [&'static crate::runtime::RuntimeSection] = &RUNTIME_ONE;
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    #[test]
    fn runtime_declarations_are_judged_like_commands_are() {
        assert!(declaration_problems::<WithRuntime>().is_empty());
        let twice = declaration_problems::<TwiceDeclared>();
        assert!(
            twice.iter().any(|p| p.contains("more than once")),
            "{twice:?}"
        );
        let service = declaration_problems::<ServiceWithRuntime>();
        assert!(
            service.iter().any(|p| p.contains("not a Cassini client")),
            "{service:?}"
        );
    }

    // -- the declaration -----------------------------------------------------------------------

    #[test]
    fn a_sound_declaration_has_no_problems() {
        assert_eq!(declaration_problems::<Example>(), Vec::<String>::new());
        assert_eq!(declaration_problems::<Bare>(), Vec::<String>::new());
    }

    struct Unsound;
    static DUPLICATE_VARS: &[&dyn Described] = &[&BUCKET];
    static DUPLICATE: Section = Section {
        name: "duplicate",
        vars: DUPLICATE_VARS,
        relations: |_| Vec::new(),
    };
    impl PolarAgent for Unsound {
        const NAME: &'static str = "";
        const PURPOSE: &'static str = "";
        const NEEDS: &'static [Capability] = &[Capability::Cassini, Capability::Cassini];
        const SHUTDOWN_BUDGET: Duration = Duration::ZERO;
        const SECTIONS: &'static [&'static Section] = &[&OWN, &DUPLICATE];
        async fn start(_: Configured) -> Result<(), StartError> {
            Ok(())
        }
    }

    #[test]
    fn an_unsound_declaration_is_reported_in_full() {
        let problems = declaration_problems::<Unsound>().join(" | ");
        for expected in [
            "name",
            "purpose",
            "budget",
            "more than once",
            "EXAMPLE_BUCKET",
        ] {
            assert!(
                problems.contains(expected),
                "missing {expected:?}: {problems}"
            );
        }
    }
}
