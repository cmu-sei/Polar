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
use polar_agent_builder::{Capability, Cause, Configured, PolarAgent, StartError, TopicDecl};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

/// This agent's own configuration topic, on which its `policy` and `groups` sections arrive and are accepted, each a
/// cause of an authority update. The builder subscribes it by rule; the declaration check refuses a cause the agent does
/// not subscribe to, so this cannot drift from `agent.<name>.configuration` unseen.
const CONFIGURATION_TOPIC: &str = "agent.credentials-agent.configuration";

pub struct CredentialsConsumer;

impl PolarAgent for CredentialsConsumer {
    const NAME: &'static str = crate::supervisor::SERVICE_NAME;
    const PURPOSE: &'static str = "Issues certificates and keeps the credentials ledger: the cluster's certificate authority front end";
    // The graph is declared as a section of its own, not required as a capability: this agent
    // projects to the graph but its health must not depend on Neo4j (the ledger is its source of
    // truth and the graph only a projection).
    const NEEDS: &'static [Capability] = &[];
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const TOPICS: &'static [TopicDecl] = &[
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_ADMIN_ACTION_REQUESTS_TOPIC,
            "a signed administrator action, relayed by the observer",
        ),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_ACCOUNT_REQUESTS_TOPIC,
            "an account request, relayed by the observer",
        ),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_ISSUANCE_REQUESTS_TOPIC,
            "a certificate issuance request, relayed by the observer",
        ),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC,
            "an identity to normalize, relayed by the observer",
        ),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC,
            "a request for its escrow public key",
        ),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_CERTIFICATE_REQUESTS_TOPIC,
            "a certificate lookup",
        )
        .waiting("nothing in the catalog publishes it yet"),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
            "a secret to store, from its owner",
        ),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC,
            "a request for a stored secret, decided by policy",
        ),
        // Since custody step 9 only a custodian asks for its workload's escrow keypair (docs/architecture/key-custody.md).
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC,
            "a custodian's request for its workload's escrow keypair",
        )
        .waiting("a custodian publishes it, and custodians are not catalog workloads: their topics are their identity's (CUSTODIAN_PUBLISHES)"),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC,
            "a custodian asking for its workload's escrow keypair to be issued",
        )
        .waiting("a custodian publishes it, and custodians are not catalog workloads: their topics are their identity's (CUSTODIAN_PUBLISHES)"),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC,
            "a request for another agent's escrow public key",
        ),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_DECRYPT_ON_BEHALF_OF_REQUESTS_TOPIC,
            "a decryption on behalf of an agent",
        )
        .waiting("nothing in the catalog publishes it yet"),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
            "a workload's certificate request, relayed by the observer",
        ),
        TopicDecl::subscribe(
            credentials_agent_common::CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC,
            "a custodian asking for one of its workload's certificates",
        )
        .waiting("a custodian publishes it, and custodians are not catalog workloads: their topics are their identity's (CUSTODIAN_PUBLISHES)"),
        TopicDecl::subscribe(
            credentials_agent_common::authority_update::AUTHORITY_SNAPSHOT_REQUESTS_TOPIC,
            "an agent asking for the current policy and permissions",
        ),
        // cert-issuer's record of every certificate it signs (docs/architecture/cert-issuer-isolation.md, "Step 2").
        TopicDecl::publish(
            cert_issuer_common::record::RECORD_REQUESTS_TOPIC,
            "asking cert-issuer for its record, after the last entry the ledger holds",
        )
        .because(&[Cause::Registered, Cause::Timer("record")]),
        TopicDecl::subscribe(
            cert_issuer_common::record::RECORD_REPLIES_TOPIC,
            "a page of cert-issuer's record, from cert-issuer alone",
        ),
        // The requests cert-issuer holds for this agent to vouch for (docs/architecture/core-restarts.md, "3 in detail").
        TopicDecl::subscribe(
            cert_issuer_common::held::HELD_REQUESTS_TOPIC,
            "a bootstrap service's request cert-issuer holds, from cert-issuer alone",
        ),
        TopicDecl::publish(
            cert_issuer_common::held::HELD_REPLIES_TOPIC,
            "this agent's answer to a request cert-issuer holds: vouched for, or refused",
        )
        .because(&[Cause::Topic(cert_issuer_common::held::HELD_REQUESTS_TOPIC)]),
        // As its orderly ending begins: its successor, the same pod restarted in place, may be signed on its token
        // (docs/architecture/core-restarts.md, "Step 5, the ticket").
        TopicDecl::publish(
            cert_issuer_common::ticket::SUCCESSOR_TICKETS_TOPIC,
            "this agent's successor ticket, left for cert-issuer as its orderly ending begins",
        )
        .of_its_own_accord("as its orderly ending begins: a rejuvenation, or a SIGTERM's drain"),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC,
            "the answer to an administrator action",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_ADMIN_ACTION_REQUESTS_TOPIC)]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_ACCOUNT_REPLIES_TOPIC,
            "the answer to an account request",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_ACCOUNT_REQUESTS_TOPIC)]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_ISSUANCE_REPLIES_TOPIC,
            "an issued certificate, or why not",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_ISSUANCE_REQUESTS_TOPIC)]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_IDENTITY_NORMALIZATION_REPLIES_TOPIC,
            "a normalized identity, or why not",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC)]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC,
            "its escrow public key",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC)]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_CERTIFICATE_REPLIES_TOPIC,
            "the answer to a certificate lookup",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_CERTIFICATE_REQUESTS_TOPIC)])
        .waiting("nothing in the catalog asks for a certificate lookup yet, so nothing reads the answer"),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_STORE_SECRET_REPLIES_TOPIC,
            "whether a secret was stored",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC)]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_RETRIEVE_SECRET_REPLIES_TOPIC,
            "a secret, encrypted to the requester, or the refusal",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC)]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
            "an agent's escrow keypair, or the refusal",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC)])
        .waiting("a custodian reads it, and custodians are not catalog workloads: their topics are their identity's (CUSTODIAN_SUBSCRIBES)"),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC,
            "an issued escrow keypair, or why not",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC)])
        .waiting("a custodian reads it, and custodians are not catalog workloads: their topics are their identity's (CUSTODIAN_SUBSCRIBES)"),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC,
            "another agent's escrow public key",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC)]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_DECRYPT_ON_BEHALF_OF_REPLIES_TOPIC,
            "the answer to a decryption on behalf of an agent",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_DECRYPT_ON_BEHALF_OF_REQUESTS_TOPIC)])
        .waiting("nothing in the catalog asks for a decryption on behalf of an agent yet, so nothing reads the answer"),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_WORKLOAD_ISSUANCE_REPLIES_TOPIC,
            "a workload's certificate, or why not",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC)]),
        TopicDecl::publish(
            credentials_agent_common::CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC,
            "a custodian's workload's certificate, or why not",
        )
        .because(&[Cause::Topic(credentials_agent_common::CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC)])
        .waiting("a custodian reads it, and custodians are not catalog workloads: their topics are their identity's (CUSTODIAN_SUBSCRIBES)"),
        TopicDecl::publish(
            credentials_agent_common::authority_update::AUTHORITY_UPDATES_TOPIC,
            "a signed change of policy, groups or revoked serials",
        )
        .because(&[
            Cause::Topic(CONFIGURATION_TOPIC),
            Cause::Topic(credentials_agent_common::CREDENTIALS_ADMIN_ACTION_REQUESTS_TOPIC),
            Cause::Registered,
        ]),
        TopicDecl::publish(
            credentials_agent_common::authority_update::AUTHORITY_SNAPSHOT_REPLIES_TOPIC,
            "the signed current policy and permissions, on request, at start and every half hour",
        )
        .because(&[
            Cause::Topic(credentials_agent_common::authority_update::AUTHORITY_SNAPSHOT_REQUESTS_TOPIC),
            Cause::Registered,
            Cause::Timer("republish"),
        ]),
    ];
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

    #[test]
    fn every_topic_it_subscribes_to_is_in_its_declared_set() {
        use polar_agent_builder::Access;
        let set = polar_agent_builder::agent::topic_set::<CredentialsConsumer>();
        for topic in credentials_agent_common::REQUEST_TOPICS
            .iter()
            .chain([&credentials_agent_common::authority_update::AUTHORITY_SNAPSHOT_REQUESTS_TOPIC])
        {
            assert!(
                set.allows(topic, Access::Subscribe),
                "{topic} is not declared"
            );
        }
    }
}
