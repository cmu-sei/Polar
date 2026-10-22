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

//! What the git observer is, as the infrastructure asks to be told: see
//! `polar_agent_builder::PolarAgent`.

use crate::settings;
use crate::supervisor::{RootSupervisor, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Cause, Configured, PolarAgent, StartError, TopicDecl};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

/// What the agent calls itself: the name a configuration or a command is addressed to.
pub const AGENT_NAME: &str = "git-repo-observer";

pub struct GitObserver;

impl PolarAgent for GitObserver {
    const NAME: &'static str = AGENT_NAME;
    const PURPOSE: &'static str =
        "Watches git repositories it is told of and publishes their commits and ref updates";
    const NEEDS: &'static [Capability] = &[Capability::Encryption];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const TOPICS: &'static [TopicDecl] = &[
        TopicDecl::subscribe(
            git_agent_common::GIT_REPO_CONFIG_EVENTS,
            "the set of repositories to observe, from the controller",
        ),
        TopicDecl::publish(
            polar::topics::GIT_REPOSITORY_EVENTS,
            "the commits and ref updates it finds",
        )
        .because(&[
            Cause::Topic(git_agent_common::GIT_REPO_CONFIG_EVENTS),
            Cause::Timer("observe"),
        ]),
        TopicDecl::publish(
            git_agent_common::status::GIT_REPO_OBSERVATION_STATUS,
            "how each attempt to observe a repository went",
        )
        .because(&[
            Cause::Topic(git_agent_common::GIT_REPO_CONFIG_EVENTS),
            Cause::Timer("observe"),
        ]),
    ];
    const SECTIONS: &'static [&'static Section] = &[&settings::SECTION];
    const RUNTIME: &'static [&'static polar_agent_builder::RuntimeSection] =
        &[&crate::credentials::CREDENTIALS];
    const HELP: Option<&'static str> = Some(
        "The git observer subscribes to repository configuration events on Cassini. For each \
         repository it is told of it clones the repository into its cache, and on the frequency \
         that event gives it fetches, walks the new commits and publishes them and the ref \
         updates. It never touches the graph. A repository that needs a credential names it; the \
         credentials come in the `credentials` runtime section, as ciphertext only this agent can \
         open, and each is used only for the hosts it is bound to.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(crate::SERVICE_NAME);

        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{}.supervisor", crate::SERVICE_NAME)),
            RootSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the git observer's supervisor did not start: {e}"))?;

        handle
            .await
            .map_err(|e| format!("the git observer's supervisor task failed: {e}"))?;

        // The supervisor stopping is never a success: an observer that no longer observes must be
        // restarted. `run` exits non-zero so the Job does.
        Err("the git observer's supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(GitObserver);
}
