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

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// Where the observer publishes each schedule change; the processor subscribes.
pub const SCHEDULER_IN_TOPIC: &str = "scheduler.in";
/// Where an ad-hoc agent announces itself with a default schedule; the processor subscribes.
pub const SCHEDULER_ADHOC_TOPIC: &str = "scheduler.adhoc";

/// The kinds of signed document, as the `type` field of a payload says (see docs/architecture/scheduling-kinds.md).
/// Every signed payload carries one, and a receiver accepts only the kind it expects.
pub mod routing;

pub mod document_type {
    /// Desired state for an agent: values for the variables its code declared Dynamic.
    pub const CONFIGURATION: &str = "configuration";
    /// What an agent works on, in sections it declared: which repositories it observes, under what rules
    /// (docs/architecture/git-controller.md). Replaced whole per section; a lower authority than
    /// configuration, since it cannot change how the agent runs.
    pub const RUNTIME: &str = "runtime";
    /// The rules under which proposals of one kind are accepted. To become the `admission` section of a
    /// runtime document.
    pub const ADMISSION: &str = "admission";
    /// A one-time instruction to an agent.
    pub const COMMAND: &str = "command";
}

/// The topic a signed configuration document for `agent` is delivered on. Every builder agent subscribes
/// to its own; the scheduler processor publishes to it.
pub fn configuration_topic(agent: &str) -> String {
    format!("agent.{agent}.configuration")
}

/// A signed configuration document on its way to the agent it is for. The document is carried as it was
/// read from the repository (`{ "payload": .., "signature": .. }`), untouched: the receiver verifies it for
/// itself, and nothing in between is trusted.
#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub struct ConfigurationNotification {
    pub agent: String,
    pub document_json: String,
}

/// Where an agent that has just registered with Cassini asks for the signed configuration the scheduler
/// holds for it. The scheduler processor subscribes and republishes the stored documents on the agent's
/// own configuration topic. Anyone can ask, and what comes back is only what the agent would have been
/// sent anyway, signed and idempotent by version, so the request needs no authority.
pub const CONFIGURATION_REPLAY_TOPIC: &str = "polar-configuration-replay";

/// "I am `agent` and I have just started or reconnected: send me what you hold for me."
#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub struct ConfigurationReplayRequest {
    pub agent: String,
}

/// The topic a signed command for `agent` is delivered on. Every builder agent subscribes to its own; the
/// scheduler processor publishes to it (docs/architecture/commands.md).
pub fn command_topic(agent: &str) -> String {
    format!("agent.{agent}.command")
}

/// A signed command document on its way to the agent it is for, carried as it was read from the repository,
/// untouched: the receiver verifies it for itself.
#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub struct CommandNotification {
    pub agent: String,
    pub document_json: String,
}

/// Where an agent says what became of each signed command it was given.
pub const COMMAND_OUTCOME_TOPIC: &str = "polar-command-outcome";

/// The values of [`CommandOutcome::status`]: one per step an agent takes. (`dispatched` and `unanswered` are
/// the scheduler's, not an agent's.)
pub mod command_status {
    /// Not accepted: unsigned, untrusted, altered, not allowed, for another agent, not declared, bad
    /// arguments, busy, or the agent is draining. The detail says which.
    pub const REFUSED: &str = "refused";
    /// Verified, but past its expiry.
    pub const EXPIRED: &str = "expired";
    /// This agent has already received this id.
    pub const DUPLICATE: &str = "duplicate";
    /// Every check passed and it will run.
    pub const ACCEPTED: &str = "accepted";
    /// The handler has begun.
    pub const STARTED: &str = "started";
    pub const SUCCEEDED: &str = "succeeded";
    /// The handler returned an error, panicked, timed out, or was interrupted by a shutdown.
    pub const FAILED: &str = "failed";
}

/// One step an agent took with a signed command. Carries names and reasons, never an argument value.
/// `id` and `command` are as the document claims them, and mean something only when `signer` is set, which
/// it is only for a document that was verified.
#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub struct CommandOutcome {
    pub agent: String,
    pub id: Option<String>,
    pub command: Option<String>,
    /// One of the [`command_status`] values.
    pub status: String,
    /// Why not, for anything that was not carried out; and for a success, what the handler reported.
    pub detail: Vec<String>,
    /// Who signed it (username or identity, and certificate serial), for a verified document.
    pub signer: Option<String>,
    /// How long the handler ran, for `succeeded` and `failed` after it started.
    pub duration_ms: Option<u64>,
    pub at_unix_secs: u64,
    /// Counts this agent's command outcomes since it started.
    pub sequence: u64,
}

/// Where an agent says what became of each signed configuration document it was given. The scheduler
/// processor subscribes and writes each to the graph.
pub const CONFIGURATION_OUTCOME_TOPIC: &str = "polar-configuration-outcome";

/// The values of [`ConfigurationOutcome::status`].
pub mod outcome {
    pub const APPLIED: &str = "applied";
    /// Verified and valid, but it changed nothing.
    pub const UNCHANGED: &str = "unchanged";
    /// Verified, but its version is not higher than the last applied.
    pub const STALE: &str = "stale";
    /// Verified, but what it asks is not acceptable (a variable that is static, a value that does not
    /// parse, a relation that would break).
    pub const INVALID: &str = "invalid";
    /// Not accepted as a signed document: unsigned, altered, untrusted, not allowed, or for another agent.
    pub const UNVERIFIED: &str = "unverified";
}

/// What an agent did with one signed configuration document. Carries names and reasons, never values.
/// `id` and `version` are as the document claims them, and mean something only when `signer` is set,
/// which it is only for a document that was verified.
#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub struct ConfigurationOutcome {
    pub agent: String,
    pub id: Option<String>,
    pub version: Option<u64>,
    /// One of the [`outcome`] values.
    pub status: String,
    /// The variables whose value changed, for an applied update.
    pub changed: Vec<String>,
    /// Why not, for anything that was not applied: problems, or the reason it was refused.
    pub detail: Vec<String>,
    /// Who signed it (username or identity, and certificate serial), for a verified document.
    pub signer: Option<String>,
    pub at_unix_secs: u64,
    /// Counts this agent's outcomes since it started, so two in the same second are two events.
    pub sequence: u64,
}

// Time specification
#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub enum TimeSpec {
    Periodic {
        interval: u64,
        unit: TimeUnit,
    },
    Exact {
        timestamp: String,
    },
    Daily {
        hour: u8,
        minute: u8,
        timezone: Option<String>,
    },
    Weekly {
        day: Weekday,
        hour: u8,
        minute: u8,
        timezone: Option<String>,
    },
    Monthly {
        day: u8,
        hour: u8,
        minute: u8,
        timezone: Option<String>,
    },
}

#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub enum TimeUnit {
    Minutes,
    Hours,
    Days,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub enum ScheduleKind {
    Permanent,
    Adhoc,
    Ephemeral,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub struct ScheduleMetadata {
    pub description: Option<String>,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct ScheduleNode {
    pub id: String,
    pub kind: ScheduleKind,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub schedule: TimeSpec,
    pub config: serde_json::Value,
    pub metadata: ScheduleMetadata,
    pub version: u64,
}

// Keep existing messages
#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub enum GitScheduleChange {
    Create { path: String, json: String },
    Update { path: String, json: String },
    Delete { path: String },
}

#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub struct AdhocAgentAnnouncement {
    pub agent_type: String,
    pub version: u64,
    pub default_schedule_json: String,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub enum ScheduleNotification {
    PermanentUpdate {
        agent_id: String,
        schedule_json: String,
    },
    AdhocUpdate {
        agent_type: String,
        schedule_json: String,
    },
    EphemeralUpdate {
        agent_type: String,
        schedule_json: String,
    },
}
