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

//! What the Cassini broker is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.
//!
//! It is the first service that is not a Cassini client, so it says so (`CASSINI_CLIENT`), turns
//! the self-test off (it has no broker to publish probes to) and is built with `ServiceBuilder`:
//! the health check, the signal listener and the shutdown handshake with its budget and watchdog
//! come from the builder, and the five-phase drain stays the broker's own.

use crate::{
    BROKER_NAME,
    broker::{Broker, BrokerArgs},
    settings,
};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

pub struct CassiniBroker;

impl PolarAgent for CassiniBroker {
    const NAME: &'static str = "cassini";
    const PURPOSE: &'static str =
        "The message broker every agent connects to over mutual TLS: sessions, topics, subscribers";
    const NEEDS: &'static [Capability] = &[];
    // The shutdown has five phases and each gets a fifth of the budget.
    const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);
    const SECTIONS: &'static [&'static Section] = &[&settings::SECTION];
    const SELF_TEST: bool = false;
    const CASSINI_CLIENT: bool = false;
    const HELP: Option<&'static str> = Some(
        "The broker accepts clients over mutual TLS, keeps their sessions, topics and \
         subscriptions, and delivers what is published. On SIGTERM, a health verdict or a \
         shutdown request over the wire (with the shutdown token) it stops accepting, drains the \
         sessions, flushes the topics and closes the subscribers and listeners, within the \
         shutdown budget.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        let settings = settings::BrokerSettings::read(configured.config())?;
        // How it logs and traces is the deployment's, in the section every agent has; the broker
        // keeps its own console format.
        let logging = polar::logging::LoggingSettings::read(configured.config())?;
        cassini_tracing::init_tracing_with(
            "cassini-broker",
            &cassini_tracing::TracingSettings {
                log_filter: Some(logging.console_filter),
                trace_filter: Some(logging.trace_filter),
                otlp_endpoint: logging.otlp_endpoint,
            },
        );
        // Required by the TLS listener when more than one provider is in the dependency tree.
        rustls::crypto::ring::default_provider()
            .install_default()
            .map_err(|_| "cannot install the rustls crypto provider")?;

        let args = BrokerArgs::from_settings(settings, configured)?;
        let (_broker, handle) = Actor::spawn(Some(BROKER_NAME.to_string()), Broker, args)
            .await
            .map_err(|e| format!("the broker did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the broker task failed: {e}"))?;
        cassini_tracing::shutdown_tracing();
        // The broker ends when its shutdown is done; `run` exits non-zero, so the Job starts a
        // fresh pod.
        Err("the broker stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(CassiniBroker);
}
