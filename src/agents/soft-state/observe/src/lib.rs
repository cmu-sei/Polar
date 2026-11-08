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

//! The state agent's reader, `soft-state-observer` (docs/architecture/soft-state.md): it answers other agents'
//! requests for soft state from the graph, deciding each by who asks (the broker's `published_by`) against the labels
//! the catalog says that workload reads, Cedar first. It reads the graph and never writes it.
//!
//! - [`floor`]: who may read which label, the signed runtime section `schema`;
//! - [`decision`]: the decision, a pure function;
//! - [`query`]: the Cypher it builds itself, from checked names;
//! - [`supervisor`]: the input and output around them.

#![deny(clippy::disallowed_methods)]

pub mod agent;
pub mod decision;
pub mod floor;
pub mod query;
pub mod settings;
pub mod supervisor;

/// The name the agent logs and traces under.
pub const SERVICE_NAME: &str = "polar.soft-state.observer";
