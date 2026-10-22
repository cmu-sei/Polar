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

//! What the OCI resolver is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::RESOLVER_SUPERVISOR_NAME;
use crate::settings;
use crate::supervisor::{ResolverSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Cause, Configured, PolarAgent, StartError, TopicDecl};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

pub struct OciResolver;

impl PolarAgent for OciResolver {
    const NAME: &'static str = crate::BROKER_CLIENT_NAME;
    const PURPOSE: &'static str = "Follows image references other agents discover to their registry and publishes the verified manifest";
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const TOPICS: &'static [TopicDecl] = &[
        TopicDecl::subscribe(
            polar::topics::PROVENANCE_DISCOVERY,
            "an image reference to resolve against its registry",
        ),
        TopicDecl::publish(
            polar::topics::KUBERNETES_RESOLUTION_EVENTS,
            "what it resolved for a Kubernetes container, for kube-consumer",
        )
        .because(&[Cause::Topic(polar::topics::PROVENANCE_DISCOVERY)]),
        TopicDecl::publish(
            polar::topics::PROVENANCE_EVENTS,
            "every artifact it resolved (manifest, digest, registry), for the provenance stream",
        )
        .because(&[Cause::Topic(polar::topics::PROVENANCE_DISCOVERY)])
        .waiting("read by build-processor, a chart outside the catalog"),
    ];
    const SECTIONS: &'static [&'static Section] = &[&settings::SECTION];
    const HELP: Option<&'static str> = Some(
        "The resolver subscribes to the provenance discovery topic on Cassini. For each image \
         reference it is told of, it pulls the manifest from the registry (with the credentials \
         in DOCKER_CONFIG, or anonymously) and publishes the resolved artifact for the build \
         processor. It never touches the graph.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(RESOLVER_SUPERVISOR_NAME);

        rustls::crypto::ring::default_provider()
            .install_default()
            .map_err(|_| "cannot install the rustls crypto provider")?;

        let (_supervisor, handle) = Actor::spawn(
            Some(RESOLVER_SUPERVISOR_NAME.to_string()),
            ResolverSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("failed to start {RESOLVER_SUPERVISOR_NAME}: {e}"))?;

        handle
            .await
            .map_err(|e| format!("{RESOLVER_SUPERVISOR_NAME} task failed: {e}"))?;

        // The supervisor stopping is never a success: a resolver that no longer resolves must be
        // restarted. `run` exits non-zero so the Job does.
        Err(format!("{RESOLVER_SUPERVISOR_NAME} stopped").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(OciResolver);
}
