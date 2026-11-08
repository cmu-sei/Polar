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

//! Canonical Cassini topic names for the Polar system.
//!
//! Topics are named after the data they carry, not the agents that consume them.
//! A topic is a logical channel — its name should describe what flows through it,
//! not which supervisor happens to be subscribed today.
//!
//! All topic strings follow the `polar.<domain>.<subject>` convention.
//! Agents must use these constants rather than hardcoding strings — a typo
//! in a topic name produces a silent failure that is very hard to debug.
//!
//! ## Current topology
//!
//! ```text
//! polar.provenance.events     ← all agents publish, all consumers subscribe
//! polar.provenance.discovery  ← observer agents publish, resolver subscribes
//! polar.git.repositories      ← git observer publishes, git processor subscribes
//! polar.agents.lifecycle      ← the operator publishes, the agent processor subscribes
//! ```

// ── Provenance ─────────────────────────────────────────────────────────────────

/// Unified provenance event stream.
///
/// All agents that observe facts about the software supply chain publish here.
/// This includes CI pipeline stages (nushell and Rust), the k8s observer,
/// the GitLab observer, and the resolver. All consumers — the build processor,
/// the provenance linker — subscribe here and filter on [`ProvenanceEvent`] variant.
///
/// Wire formats accepted on this topic:
/// - rkyv-serialized [`ProvenanceEvent`] (Rust agents)
/// - JSON-serialized [`ProvenanceEvent`] (nushell pipeline stages)
pub const PROVENANCE_EVENTS: &str = "polar.provenance.events";

/// Provenance discovery requests.
///
/// Observer agents publish [`ProvenanceEvent::OCIArtifactDiscovered`] and
/// [`ProvenanceEvent::ImageRefDiscovered`] here to trigger resolution.
/// The resolver subscribes exclusively to this topic, fetches manifests,
/// and publishes [`ProvenanceEvent::OCIArtifactResolved`] and
/// [`ProvenanceEvent::ImageRefResolved`] back to [`PROVENANCE_EVENTS`].
pub const PROVENANCE_DISCOVERY: &str = "polar.provenance.discovery";

// ── Git ────────────────────────────────────────────────────────────────────────

/// Git repository observation events.
///
/// The git observer publishes [`GitRepositoryUpdatedEvent`] and
/// [`GitRepositoryDiscoveredEvent`] here. The git processor and any
/// agent that triggers on new commits subscribes here.
pub const GIT_REPOSITORY_DISCOVERED: &str = "polar.git.repositories.discovery";

pub const GIT_REPOSITORY_EVENTS: &str = "polar.git.repositories.events";

// ── Agents ─────────────────────────────────────────────────────────────────────

/// Agent lifecycle events.
///
/// The operator publishes [`crate::AgentLifecycleEvent`]s here (rkyv): what each
/// agent is declared to be, its phases, and every instance it starts and sees
/// end. The agent processor subscribes and writes canonical `PolarAgent` nodes.
pub const AGENT_LIFECYCLE_EVENTS: &str = "polar.agents.lifecycle";

// TODO: Replace "cluster" with a cluster id. Chances are we won't get away wtih constants the moment someone wants to observe multiple clusters
pub const KUBERNETES_RESOLUTION_EVENTS: &str = "polar.kubernetes.cluster.resolution";
