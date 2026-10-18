//! The `PolarAgent` resource: one long-lived service that follows the agent contract.
//!
//! The spec is what the deployment gives the agent. What the agent's code requires is the
//! agent's own declaration (`--scaffold`), and the agent refuses to start when the two
//! disagree; the operator does not repeat that judgement, it records the refusal.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The default least time an agent is given to drain.
pub const DEFAULT_SHUTDOWN_BUDGET_SECS: u32 = 20;

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, PartialEq, JsonSchema)]
#[kube(
    group = "polar.sei.cmu.edu",
    version = "v1alpha1",
    kind = "PolarAgent",
    namespaced,
    status = "PolarAgentStatus",
    shortname = "pagent",
    printcolumn = r#"{"name":"Agent","type":"string","jsonPath":".spec.agent","priority":1}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Instance","type":"string","jsonPath":".status.pod"}"#,
    printcolumn = r#"{"name":"Started","type":"integer","jsonPath":".status.instancesStarted"}"#,
    printcolumn = r#"{"name":"Last ending","type":"string","jsonPath":".status.lastEnding.kind"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct PolarAgentSpec {
    /// The agent this deploys, by the name its code declares (`PolarAgent::NAME`), so that
    /// one agent deployed in several places is one agent. Defaults to this resource's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The image the agent runs. Pin it by digest outside of development.
    pub image: String,
    /// Overrides the image's entrypoint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Variables beyond the ones the operator derives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<AgentEnv>,
    /// The capabilities this deployment is configured to give the agent, passed to it as
    /// `POLAR_CAPABILITIES`. A configuration, not an assertion: a builder agent judges it
    /// against what its code requires before it initializes, and refuses on a mismatch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    /// How long the agent may take to drain once told to stop. The pod's grace period is
    /// derived from it and is never set on its own.
    #[serde(default = "default_budget")]
    pub shutdown_budget_secs: u32,
    /// The identity the agent runs as; the name on its certificate. Defaults to this
    /// resource's name, so that two deployments of one agent never share one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_account: Option<String>,
    /// Stop the agent and start nothing until this is cleared.
    #[serde(default)]
    pub suspend: bool,
}

impl PolarAgent {
    /// The agent this resource deploys: `spec.agent`, or else the resource's name.
    pub fn agent_name(&self) -> String {
        self.spec
            .agent
            .clone()
            .unwrap_or_else(|| self.metadata.name.clone().unwrap_or_default())
    }
}

fn default_budget() -> u32 {
    DEFAULT_SHUTDOWN_BUDGET_SECS
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct AgentEnv {
    pub name: String,
    pub value: String,
}

/// What the operator knows about an agent's instances. It is also the operator's memory:
/// the backoff and the crash-loop count survive an operator restart because they live here.
///
/// Every field is always serialized, `None` as `null`: the status is written with a merge
/// patch, and a field left out of one keeps its old value.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PolarAgentStatus {
    #[serde(default)]
    pub phase: Phase,
    /// The current instance's pod, when there is one.
    #[serde(default)]
    pub pod: Option<String>,
    /// The hash of the spec the operator last acted on.
    #[serde(default)]
    pub spec_hash: Option<String>,
    /// How many instances have been started, ever. The next pod is named from it.
    #[serde(default)]
    pub instances_started: u64,
    /// Endings in a row that came too soon after a start, for the spec in `failingHash`.
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub failing_hash: Option<String>,
    /// No instance is started before this time (RFC 3339).
    #[serde(default)]
    pub next_start_after: Option<String>,
    #[serde(default)]
    pub last_ending: Option<Ending>,
    /// For a person: what the operator is doing or waiting on.
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum Phase {
    /// Nothing decided yet.
    #[default]
    Pending,
    /// An instance exists and has not started running.
    Starting,
    Running,
    /// The instance is being stopped so that one from a new spec can start.
    Replacing,
    /// The last instance ended and its replacement waits for the old pod to be gone.
    Ending,
    /// Waiting out the backoff before the next start.
    BackingOff,
    /// Held: too many endings in a row soon after starting. A change to the spec releases it.
    CrashLoop,
    Suspended,
}

/// How an instance ended, as far as the cluster can tell.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Ending {
    pub pod: String,
    /// The pod's UID: the same `Pod` node the kube consumer writes.
    #[serde(default)]
    pub pod_uid: Option<String>,
    pub kind: EndingKind,
    pub exit_code: i32,
    /// Kubernetes' reason (`Error`, `OOMKilled`, `Completed`, `Evicted`, ...).
    pub reason: String,
    /// The instance's own account: its termination message, or its last log lines when it
    /// wrote none.
    #[serde(default)]
    pub message: Option<String>,
    /// How long it ran, in seconds.
    pub ran_secs: i64,
    pub finished_at: String,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
pub enum EndingKind {
    /// Exit 1 or another non-zero code it chose: the contract's way to ask for a replacement.
    Exited,
    /// Exit 0. An agent should never; a service that does not follow the contract might.
    Completed,
    /// Killed by a signal it did not handle in time (137): wedged, or a budget too short.
    Killed,
    OomKilled,
    /// The pod was removed from under it (eviction, a lost node).
    Lost,
}
