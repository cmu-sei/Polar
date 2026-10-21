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

//! What the credentials agent consumer is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.
//!
//! Its settings are a section of its own (`settings.rs`), read from the snapshot the builder took.

use crate::supervisor::{ConsumerSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

pub struct CredentialsConsumer;

impl PolarAgent for CredentialsConsumer {
    const NAME: &'static str = crate::supervisor::SERVICE_NAME;
    const PURPOSE: &'static str = "Issues certificates and keeps the credentials ledger: the cluster's certificate authority front end";
    // The graph is declared as a section of its own, not required as a capability: this agent
    // projects to the graph but its health must not depend on Neo4j (the ledger is its source of
    // truth and the graph only a projection).
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const SECTIONS: &'static [&'static Section] = &[
        &polar::graph::environment::SECTION,
        &crate::settings::SECTION,
    ];
    const RUNTIME: &'static [&'static polar_agent_builder::RuntimeSection] = &[
        &crate::authority_sections::POLICY,
        &crate::authority_sections::GROUPS,
    ];
    const HELP: Option<&'static str> = Some(
        "The credentials agent consumer answers the certificate, identity, account, secret and \
         escrow-key requests the cluster's agents make over Cassini. The ledger is its source of \
         truth, and the graph a projection of it.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(crate::supervisor::SERVICE_NAME);
        let (_agent, handle) = Actor::spawn(
            Some(format!("{}.supervisor", crate::supervisor::SERVICE_NAME)),
            ConsumerSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the consumer supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the consumer supervisor task failed: {e}"))?;
        // The supervisor is meant to live as long as the pod; `run` exits non-zero, so the Job
        // starts a fresh pod, which reruns polar-init and rebuilds every client from fresh
        // certificates.
        Err("the consumer supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(CredentialsConsumer);
}
