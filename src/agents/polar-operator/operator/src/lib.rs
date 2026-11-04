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

//! polar-operator: the lifecycle of anything that follows the Polar agent contract.
//!
//! A `PolarAgent` resource says what should be running: one image, the capabilities
//! the deployment provides it, the least time it needs to drain, and the identity it
//! runs as. The operator keeps exactly one instance of it alive, and never two.
//!
//! The contract it relies on is the one `polar-agent-builder` gives every agent, and
//! which any service can follow:
//!
//! - an instance never ends on purpose with 0: every ending asks for a replacement;
//! - it says why it ended in its termination message (`/dev/termination-log`);
//! - it drains within its shutdown budget once told to stop (SIGTERM).
//!
//! A container that knows nothing of the contract still runs; its endings are classified
//! from the exit code and the last lines of its log instead.
//!
//! Layout, following the builder's "decisions without actors":
//!
//! - [`crd`]: the resource and its status (from `polar-operator-common`).
//! - [`lifecycle`]: what to do, as a pure function of what is desired and what is
//!   observed. No cluster, no clock of its own.
//! - [`pod`]: the pod an agent runs as, and what a pod says about its instance.
//! - [`controller`]: the kube-rs loop that observes, asks [`lifecycle::decide`], and acts.
//! - [`events`]: the lifecycle events each pass produces, published to Cassini.
//! - [`supervisor`]: the actor that owns the Cassini client and runs the controller.

pub mod controller;
pub mod events;
pub mod lifecycle;
pub mod pod;
pub mod supervisor;

/// The resource, shared with the kube agents that observe it.
pub use polar_operator_common::crd;
pub use polar_operator_common::{AGENT_LABEL, GROUP, SPEC_HASH_LABEL};

/// The field manager the operator writes as.
pub const MANAGER: &str = "polar-operator";
/// The operator's name as a Cassini client.
pub const OPERATOR_NAME: &str = "polar.operator";
