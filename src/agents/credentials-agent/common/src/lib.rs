/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

pub mod account_claim;
pub mod admin_action;
pub mod admin_signing;
pub mod agent_escrow_issuance;
pub mod agent_escrow_key;
pub mod agent_escrow_keypair_bootstrap;
pub mod authority_book;
pub mod authority_state;
pub mod authority_update;
pub mod certificate;
pub mod decrypt_on_behalf_of;
pub mod ephemeral_exchange;
pub mod escrow_key;
pub mod escrow_secret;
pub mod events;
pub mod graph;
pub mod identity;
pub mod issuance;
pub mod observer;
pub mod retrieve_secret;
pub mod secret;
pub mod workload_issuance;

/// Published by an admin's tool (polar-admin-grant) to grant or
/// revoke permissions -- see events.rs's own CredentialsEvent::Grant/
/// Revoke. Fire-and-forget from the publisher's side; no reply topic,
/// since these are commands, not queries.
/// Signed admin actions (provision, grant, revoke, policy update) --
/// see admin_action.rs. A request/reply pair, so a refusal (bad
/// signature, missing capability, replay) reaches the admin's tool.
/// Replaces the three unauthenticated topics below once the
/// consumer's signed-action handler lands.
pub const CREDENTIALS_ADMIN_ACTION_REQUESTS_TOPIC: &str = "credentials-admin-action-requests";
pub const CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC: &str = "credentials-admin-action-replies";
pub const CREDENTIALS_ACCOUNT_REQUESTS_TOPIC: &str = "credentials-account-requests";
pub const CREDENTIALS_ACCOUNT_REPLIES_TOPIC: &str = "credentials-account-replies";
pub const CREDENTIALS_GRANTS_TOPIC: &str = "credentials-grants";
pub const CREDENTIALS_REVOCATIONS_TOPIC: &str = "credentials-revocations";
pub const CREDENTIALS_ACCOUNT_PROVISIONING_TOPIC: &str = "credentials-account-provisioning";

/// The human self-service and admin-bootstrap issuance lifecycles --
/// see issuance.rs's own IssuanceRequest/Reply. Proxied through the
/// observer for external, human-facing callers, unlike every other
/// query/issuance pair below, which are core-to-core only.
pub const CREDENTIALS_ISSUANCE_REQUESTS_TOPIC: &str = "credentials-issuance-requests";
pub const CREDENTIALS_ISSUANCE_REPLIES_TOPIC: &str = "credentials-issuance-replies";

// A workload's own request for its pod-instance certificates, with the token sealed to an ephemeral session
// (`workload_issuance.rs`): the observer relays it and the consumer opens it.
pub const CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC: &str =
    "credentials-workload-issuance-requests";
pub const CREDENTIALS_WORKLOAD_ISSUANCE_REPLIES_TOPIC: &str =
    "credentials-workload-issuance-replies";

/// What SAN a CSR should carry for a given credential's identity --
/// see identity.rs's own IdentityNormalizationRequest/Reply. Queried
/// before a CSR is ever built, so its SAN already matches what
/// cert-issuer's /issue will independently check.
pub const CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC: &str =
    "credentials-identity-normalization-requests";
pub const CREDENTIALS_IDENTITY_NORMALIZATION_REPLIES_TOPIC: &str =
    "credentials-identity-normalization-replies";

/// The Credentials Agent's own, singleton escrow public key -- see
/// escrow_key.rs's own EscrowKeyRequest/Reply. What any participant
/// encrypts a secret to before handing it off for storage. Distinct
/// from CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC below, which asks
/// about a specific *other agent's* own key, not this agent's own.
pub const CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC: &str = "credentials-escrow-key-requests";
pub const CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC: &str = "credentials-escrow-key-replies";

/// A specific identity's current mTLS client certificate PEM -- see
/// certificate.rs's own CertificateRequest/Reply. Lets one agent
/// encrypt a payload directly to another agent's own, currently-valid
/// pod-instance identity.
pub const CREDENTIALS_CERTIFICATE_REQUESTS_TOPIC: &str = "credentials-certificate-requests";
pub const CREDENTIALS_CERTIFICATE_REPLIES_TOPIC: &str = "credentials-certificate-replies";

/// Store half of the general secret-escrow mechanism -- see secret.rs's
/// own StoreSecretRequest/Reply. Retrieve doesn't have a topic pair
/// yet; see credentials-agent-architecture.md's own "What is
/// deferred."
pub const CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC: &str = "credentials-store-secret-requests";
pub const CREDENTIALS_STORE_SECRET_REPLIES_TOPIC: &str = "credentials-store-secret-replies";

/// The third issuance lifecycle -- automated agent escrow-identity
/// issuance -- see agent_escrow_issuance.rs's own
/// AgentEscrowIssuanceRequest/Reply. Core-to-core only, unlike human
/// issuance above: an agent's own service identity is never an
/// external, human-facing caller, so this is never proxied through
/// the observer.
pub const CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC: &str =
    "credentials-agent-escrow-issuance-requests";
pub const CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC: &str =
    "credentials-agent-escrow-issuance-replies";

/// A specific *other agent's* own escrow public key -- see
/// agent_escrow_key.rs's own AgentEscrowKeyRequest/Reply. What
/// CryptoActor itself queries (and caches) on behalf of the
/// "encrypt and send" flow, to encrypt a payload to that agent's
/// durable service identity rather than its ephemeral mTLS
/// certificate. Distinct from CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC
/// above, which asks about the Credentials Agent's own key
/// specifically.
pub const CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC: &str =
    "credentials-agent-escrow-key-requests";
pub const CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC: &str = "credentials-agent-escrow-key-replies";

/// Retrieve half of the general secret-escrow mechanism -- see
/// retrieve_secret.rs's own RetrieveSecretRequest/Reply. The
/// general-case relay, re-encrypting to the requester's own,
/// already-stable mTLS certificate; not the one bootstrap case (an
/// agent retrieving its own escrow keypair for the first time) that
/// needs the separate ephemeral X25519 exchange instead.
pub const CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC: &str = "credentials-retrieve-secret-requests";
pub const CREDENTIALS_RETRIEVE_SECRET_REPLIES_TOPIC: &str = "credentials-retrieve-secret-replies";

/// The one, narrow bootstrap case CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC
/// above doesn't cover -- see agent_escrow_keypair_bootstrap.rs's own
/// RetrieveAgentEscrowKeypairRequest/Reply and its own module doc for
/// why this needs a genuinely separate mechanism (the ephemeral
/// X25519 exchange) rather than the general retrieve flow.
pub const CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC: &str =
    "credentials-retrieve-agent-escrow-keypair-requests";
pub const CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC: &str =
    "credentials-retrieve-agent-escrow-keypair-replies";

/// Decrypting something a *different* agent encrypted -- see
/// decrypt_on_behalf_of.rs's own DecryptOnBehalfOfRequest/Reply and
/// its own module doc for the complete, real reasoning. Genuinely, a
/// real authorization decision, not merely a retrieve: unlike
/// CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC above (where the
/// requester is always authorized), this requester is genuinely,
/// potentially a stranger to whatever originally encrypted the
/// ciphertext being decrypted.
pub const CREDENTIALS_DECRYPT_ON_BEHALF_OF_REQUESTS_TOPIC: &str =
    "credentials-decrypt-on-behalf-of-requests";
pub const CREDENTIALS_DECRYPT_ON_BEHALF_OF_REPLIES_TOPIC: &str =
    "credentials-decrypt-on-behalf-of-replies";
