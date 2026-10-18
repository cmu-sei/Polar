//! The `PolarAgent` resource: what the operator reconciles and the kube agents observe.

pub mod crd;

pub use crd::{AgentEnv, Ending, EndingKind, Phase, PolarAgent, PolarAgentSpec, PolarAgentStatus};

/// The API group of every resource the operator owns.
pub const GROUP: &str = "polar.sei.cmu.edu";
/// The resource's kind, as a pod's owner reference and the kube agents name it.
pub const KIND: &str = "PolarAgent";
/// Label on every pod: the `PolarAgent` resource (the deployment) it is an instance of.
pub const AGENT_LABEL: &str = "polar.sei.cmu.edu/agent";
/// Label on every pod: the hash of the spec it was made from.
pub const SPEC_HASH_LABEL: &str = "polar.sei.cmu.edu/spec-hash";
