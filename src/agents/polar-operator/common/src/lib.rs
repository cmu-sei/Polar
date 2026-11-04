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
