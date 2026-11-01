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

//! The state agent's writer, `soft-state-processor` (docs/architecture/soft-state.md, "As built: pulled health"): on
//! its interval it asks every workload its signed runtime section `agents` names, on that workload's own topic
//! `agent.<workload>.health`, alone hears the answers on `polar-health-replies`, and writes each workload's
//! `(:AgentHealth)`: the latest report and the last transitions. It writes only that label.
//!
//! - [`expected`]: who is asked, the signed runtime section `agents`;
//! - [`classify`]: answering, unresponsive or unreachable, a pure function;
//! - [`history`]: the last transitions, kept in order and bounded;
//! - [`graph`]: what is written, and read back at start;
//! - [`supervisor`]: the rounds, around them.

#![deny(clippy::disallowed_methods)]

pub mod agent;
pub mod classify;
pub mod expected;
pub mod graph;
pub mod history;
pub mod settings;
pub mod supervisor;

/// The name the agent logs and traces under.
pub const SERVICE_NAME: &str = "polar.soft-state.processor";
