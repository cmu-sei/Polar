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

pub mod events;
pub mod issuance;
pub mod observer;
pub mod identity;
pub mod graph;
pub mod certificate;
pub mod escrow_key;
pub mod escrow_secret;
pub mod secret;
pub mod agent_escrow_issuance;
pub mod agent_escrow_key;

/// Published by an admin's tool (polar-admin-grant) to grant or
/// revoke permissions -- see events.rs's own CredentialsEvent::Grant/
/// Revoke. Fire-and-forget from the publisher's side; no reply topic,
/// since these are commands, not queries.
pub const CREDENTIALS_GRANTS_TOPIC: &str = "credentials-grants";
pub const CREDENTIALS_REVOCATIONS_TOPIC: &str = "credentials-revocations";

/// The human self-service and admin-bootstrap issuance lifecycles --
/// see issuance.rs's own IssuanceRequest/Reply. Proxied through the
/// observer for external, human-facing callers, unlike every other
/// query/issuance pair below, which are core-to-core only.
pub const CREDENTIALS_ISSUANCE_REQUESTS_TOPIC: &str = "credentials-issuance-requests";
pub const CREDENTIALS_ISSUANCE_REPLIES_TOPIC: &str = "credentials-issuance-replies";

/// What SAN a CSR should carry for a given credential's identity --
/// see identity.rs's own IdentityNormalizationRequest/Reply. Queried
/// before a CSR is ever built, so its SAN already matches what
/// cert-issuer's /issue will independently check.
pub const CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC: &str = "credentials-identity-normalization-requests";
pub const CREDENTIALS_IDENTITY_NORMALIZATION_REPLIES_TOPIC: &str = "credentials-identity-normalization-replies";

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
pub const CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC: &str = "credentials-agent-escrow-key-requests";
pub const CREDENTIALS_AGENT_ESCROW_KEY_REPLIES_TOPIC: &str = "credentials-agent-escrow-key-replies";
