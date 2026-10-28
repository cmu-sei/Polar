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

//! HTTP handler for `POST /issue`.
//!
//! Wires the OIDC validator, CSR parser, and CA client together.
//! The handler is the integration point where the protocol's
//! end-to-end behavior is specified.
//!
//! # Order of operations
//!
//! Each step short-circuits on failure. The CA is never called
//! unless OIDC validation, CSR parsing, and identity verification
//! all succeed. This is a security property, not just an
//! optimization: a failed token validation must not consume a CA
//! request slot, must not appear in CA audit logs, and must not
//! produce any side effect downstream of the cert issuer.
//!
//! 1. **OIDC validation.** Returns `InvalidToken`, `InvalidAudience`,
//!    `InvalidIssuer(_)`, `Expired`, `MissingClaim(_)`, or
//!    `ForbiddenAlgorithm(_)`. All of these short-circuit; no further
//!    work happens.
//! 2. **CSR parsing.** Returns `InvalidCsr` on any parse failure
//!    (malformed PEM, bad signature, multi-SAN, etc.).
//! 3. **Identity match.** The CSR's SAN must equal the token's
//!    workload identity. Mismatch returns `IdentityMismatch`.
//! 4. **Session ID generation.** Fresh UUID per request, used for
//!    audit correlation between this issuance event and downstream
//!    mTLS handshake logs.
//! 5. **CA call.** Signs the CSR with the in-process rcgen CA.
//!    `CaError::Malformed` surfaces as 400; `CaError::Internal`
//!    surfaces as 500.
//!
//! # Why bearer token is a parameter, not extracted here
//!
//! `Handler::handle` takes the raw bearer token as a parameter so
//! the handler is testable without going through HTTP. The Axum
//! layer (in `crate::server`) extracts the `Authorization` header
//! and passes the token in. Tests bypass the Axum layer entirely.

use crate::ca::{CaClient, CaError, CaIssueRequest};
use crate::csr;
use crate::oidc::{ValidationError, Validator};
use cert_issuer_common::ValidateResponse;
use cert_issuer_common::{
    InternalIssueRequest, IssueError, IssueOutcome, IssueRequest, IssueResponse, ValidateRequest,
    identity::normalize_identity,
};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, instrument, warn};

pub struct Handler {
    pub validator: Arc<Validator>,
    pub ca: Arc<dyn CaClient>,
    /// Lifetime for client certs.
    pub default_lifetime: Duration,
    /// Lifetime for server certs. Typically longer than client certs
    /// to reduce Cassini restart frequency.
    pub server_lifetime: Duration,
    /// Lifetime for account (user) certificates, issued with an
    /// `account_subject`. Long by design: it is what a person's browser holds.
    pub user_lifetime: Duration,
    /// Lifetime for an agent's escrow certificate (`CertType::Service`): long, since it outlives the agent's pods.
    pub service_lifetime: Duration,
    /// cert-issuer's own workload, as its own token names it, once it has signed itself its client certificate
    /// (`handle_own`): signed in process at its start, and refused to every caller over HTTP.
    pub own_workload: std::sync::OnceLock<String>,
    /// The floor's gate on token-only requests: the floor state and the requests held (`gate`), fed by the
    /// supervisor; none in a handler that is not running as the agent, and then the floor alone decides.
    pub gate: Option<Arc<crate::gate::FloorGate>>,
    pub identity_lifetime_overrides: std::collections::HashMap<String, Duration>,
    /// Peer identities (the SAN of the connection's verified mTLS
    /// client certificate) allowed to call `handle_internal_issue`.
    /// Empty means that endpoint refuses every caller, which is the
    /// safe default until credentials-agent has its own identity.
    pub internal_callers: Vec<String>,
    /// The bootstrap identities: the only callers that may be issued with nothing but a token. With a floor
    /// set, any other token-only caller is refused on `/issue` and `/validate`; a caller vouched for by an
    /// internal-caller mTLS identity is not limited by it. `None` is the old open behaviour (any token holder
    /// is signed what it asks for), kept only until `polar-init` goes through the credentials agent.
    pub floor: Option<Vec<crate::floor::BootstrapIdentity>>,
    /// PEM of the CA certificate this service signs with, served
    /// unauthenticated at `GET /ca`. Public by nature: every issuance
    /// response already carries it, and it is presented in every TLS
    /// handshake. It exists so a caller on its very first contact can
    /// obtain the trust root it has no other way to get.
    pub ca_cert_pem: String,
}

#[derive(Debug)]
pub struct HandlerResponse {
    pub status: u16,
    pub body: HandlerBody,
}

#[derive(Debug)]
pub enum HandlerBody {
    Success(IssueResponse),
    Error(IssueError),
}

#[derive(Debug)]
pub enum ValidateHandlerBody {
    Success(ValidateResponse),
    Error(IssueError),
}

#[derive(Debug)]
pub struct ValidateHandlerResponse {
    pub status: u16,
    pub body: ValidateHandlerBody,
}

/// How the floor's gate let a token-only request through.
enum Gated {
    /// On its token: no floor, or the floor state has its entry open.
    Token,
    /// The credentials agent vouched for it, held.
    Vouched(crate::held::HeldKey),
    /// Against the credentials agent's successor ticket.
    Ticketed,
}

impl Handler {
    /// Whether the connection's verified mTLS peer is one that vouches for the request: an internal caller
    /// (the credentials agent), which has applied its own controls and states the certificate itself.
    fn vouched_for(&self, peer_identity: Option<&str>) -> bool {
        peer_identity.is_some_and(|peer| self.internal_callers.iter().any(|c| c == peer))
    }

    /// Whether a token-only request for a bootstrap identity, which the floor admits, goes through now: `Ok(None)` if
    /// the floor state has its entry open (or the request is not one the state decides), `Ok(Some(key))` if it was
    /// held and the credentials agent has vouched for it, and otherwise the answer to give (held, or refused).
    fn gate_on_token(
        &self,
        own: bool,
        peer_identity: Option<&str>,
        claims: &crate::oidc::ValidatedClaims,
        dns_identity: &str,
        request: &IssueRequest,
    ) -> Result<Gated, HandlerResponse> {
        use crate::held::{Asked, HeldKey};
        let (false, false, Some(gate), Some(_)) = (
            own,
            self.vouched_for(peer_identity),
            &self.gate,
            &self.floor,
        ) else {
            return Ok(Gated::Token);
        };
        let now = std::time::Instant::now();
        let workload = &claims.workload_identity;
        // The credentials agent's custodian waits as the credentials agent does (docs/architecture/key-custody.md,
        // "Part A's bootstrap: the custodian").
        let service =
            cert_issuer_common::custody::custodian_of(dns_identity).unwrap_or(dns_identity);
        let credentials_agent = self.internal_callers.iter().any(|c| c == service);
        // The credentials agent's successor, the same pod restarted in place, against its predecessor's ticket
        // (docs/architecture/core-restarts.md, "Step 5, the ticket"), before any window.
        if credentials_agent
            && gate.admit_ticket(
                claims.instance_binding.as_deref(),
                &request.cert_type,
                time::OffsetDateTime::now_utc().unix_timestamp(),
            )
        {
            info!(workload = %workload, cert_type = ?request.cert_type, "admitted on its token against the credentials agent's successor ticket");
            return Ok(Gated::Ticketed);
        }
        let open = gate.open(now);
        if open.admits(credentials_agent) {
            info!(workload = %workload, floor_state = ?open, "admitted on its token: the floor state has its entry open");
            return Ok(Gated::Token);
        }
        if credentials_agent {
            // Its successor is vouched for by no one: it waits until the state opens its entry.
            info!(workload = %workload, floor_state = ?open, "held: the credentials agent's entry is not open yet");
            return Err(error_response(
                IssueOutcome::Held,
                "the credentials agent's entry is not open yet; ask again shortly",
            ));
        }
        let key = HeldKey {
            workload: workload.clone(),
            instance_binding: claims.instance_binding.clone(),
            cert_type: request.cert_type.clone(),
            csr_pem: request.csr_pem.clone(),
        };
        let offer = cert_issuer_common::held::HeldOffer {
            id: String::new(),
            workload: workload.clone(),
            identity: dns_identity.to_string(),
            instance_binding: claims.instance_binding.clone(),
            cert_type: request.cert_type.clone(),
            names: request.extra_sans.clone(),
            csr_pem: request.csr_pem.clone(),
        };
        match gate.ask(key.clone(), offer, now) {
            Asked::Vouched(id) => {
                info!(workload = %workload, held = %id, "signed: the credentials agent vouched for the held request");
                Ok(Gated::Vouched(key))
            }
            Asked::Refused(reason) => {
                warn!(workload = %workload, reason = %reason, "refused: the credentials agent refused the held request");
                Err(error_response(
                    IssueOutcome::CallerNotAuthorized,
                    &format!("the credentials agent refused this request: {reason}"),
                ))
            }
            Asked::New(offer) => {
                info!(workload = %workload, held = %offer.id, floor_state = ?open, "held, and offered to the credentials agent");
                Err(error_response(
                    IssueOutcome::Held,
                    "held until the credentials agent vouches for it; ask again shortly",
                ))
            }
            Asked::Waiting(_) => Err(error_response(
                IssueOutcome::Held,
                "held until the credentials agent vouches for it; ask again shortly",
            )),
            Asked::Full => Err(error_response(
                IssueOutcome::Held,
                "too many requests are held; ask again shortly",
            )),
        }
    }

    /// `GET /internal/held`: the held requests not yet answered, for the credentials agent while its Cassini session
    /// is lost. Only an internal caller is answered, and its asking is its presence. `Err` is why it is not answered.
    pub fn held_list(
        &self,
        peer_identity: Option<&str>,
    ) -> Result<cert_issuer_common::held::HeldList, &'static str> {
        if !self.vouched_for(peer_identity) {
            return Err("only the credentials agent may list the held requests");
        }
        let now = std::time::Instant::now();
        let held = match &self.gate {
            Some(gate) => {
                gate.hear(crate::floor_state::Heard::CredentialsAgent, now);
                gate.waiting(now)
            }
            None => Vec::new(),
        };
        Ok(cert_issuer_common::held::HeldList { held })
    }

    /// `POST /internal/held/<id>`: the credentials agent's answer to a held request, over mTLS. `Ok(taken)` says
    /// whether it was taken (a request held and not yet answered).
    pub fn held_answer(
        &self,
        peer_identity: Option<&str>,
        reply: &cert_issuer_common::held::HeldReply,
    ) -> Result<bool, &'static str> {
        if !self.vouched_for(peer_identity) {
            return Err("only the credentials agent may answer a held request");
        }
        let Some(gate) = &self.gate else {
            return Ok(false);
        };
        gate.hear(
            crate::floor_state::Heard::CredentialsAgent,
            std::time::Instant::now(),
        );
        Ok(gate.answer(reply))
    }

    /// Why a caller with this token may not have this request served, if it may not: the detail to send. Once a
    /// floor is configured, the names a request carries are checked on every path against what is written down for
    /// the token's workload. A vouched caller is not limited otherwise (the credentials agent has applied its own
    /// controls and states the type and permissions); a token-only caller must also be a bootstrap identity asking
    /// only for its own type. `request` is the type and names for `/issue`, and none for `/validate`, which signs
    /// nothing.
    fn caller_refusal(
        &self,
        peer_identity: Option<&str>,
        workload_identity: &str,
        request: Option<(&cert_issuer_common::CertType, &[String])>,
    ) -> Option<String> {
        let floor = self.floor.as_ref();
        let vouched = self.vouched_for(peer_identity);
        let refusal = match (vouched, floor, request) {
            (_, None, _) => return None,
            (true, Some(floor), Some((cert_type, extra_sans))) => {
                crate::floor::admit_vouched(floor, workload_identity, cert_type, extra_sans)
            }
            (true, Some(_), None) => return None,
            (false, Some(floor), Some((cert_type, extra_sans))) => {
                crate::floor::admit_token_only(floor, workload_identity, cert_type, extra_sans)
            }
            (false, Some(floor), None)
                if floor
                    .iter()
                    .any(|b| b.workload == workload_identity && !b.vouched_only) =>
            {
                Ok(())
            }
            (false, Some(_), None) => Err(crate::floor::Refusal::NotBootstrap),
        };
        match refusal {
            Ok(()) => None,
            Err(refusal) => {
                warn!(
                    workload = %workload_identity,
                    peer_identity = ?peer_identity,
                    "refused a request: {refusal}"
                );
                Some(refusal.to_string())
            }
        }
    }

    /// Process an `/issue` request.
    // skip_all: the bearer token is a live credential and the request carries
    // the CSR. Neither belongs in a span field, and the debug log would
    // otherwise print both in full.
    #[instrument(skip_all)]
    pub async fn handle(
        &self,
        peer_identity: Option<&str>,
        bearer_token: &str,
        request: IssueRequest,
    ) -> HandlerResponse {
        self.issue_on_token(peer_identity, bearer_token, request, false)
            .await
    }

    /// cert-issuer's own client certificate, on its own token, signed in process at its start (never asked for over
    /// HTTP): its floor entry decides the type and the names, as anyone's does, and from then its workload is its own,
    /// refused to every caller of `handle` (docs/architecture/core-restarts.md, "2 in detail").
    #[instrument(skip_all)]
    pub async fn handle_own(&self, own_token: &str, request: IssueRequest) -> HandlerResponse {
        self.issue_on_token(None, own_token, request, true).await
    }

    async fn issue_on_token(
        &self,
        peer_identity: Option<&str>,
        bearer_token: &str,
        request: IssueRequest,
        own: bool,
    ) -> HandlerResponse {
        // ---- Step 1: OIDC validation ----
        let claims = match self.validator.validate(bearer_token).await {
            Ok(claims) => claims,
            Err(e) => return error_response(map_validation_error(&e), &e.to_string()),
        };

        // ---- Step 1.1: cert-issuer's own identity is its own ----
        if own {
            let _ = self.own_workload.set(claims.workload_identity.clone());
            if self.own_workload.get() != Some(&claims.workload_identity) {
                return error_response(
                    IssueOutcome::CallerNotAuthorized,
                    "cert-issuer has signed itself as another workload already",
                );
            }
        } else if self.own_workload.get() == Some(&claims.workload_identity) {
            warn!(workload = %claims.workload_identity, peer_identity = ?peer_identity, "a certificate as cert-issuer was asked for over HTTP: refused");
            return error_response(
                IssueOutcome::CallerNotAuthorized,
                "cert-issuer's own identity is signed only by cert-issuer itself, at its start",
            );
        }

        // ---- Step 1.2: a signing certificate is not issued here ----
        //
        // It is the internal caller's own, asked for on /internal/issue: a token proves whose workload this is, and
        // the credentials agent vouches for workloads, never for a key to sign as one with.
        if request.cert_type == cert_issuer_common::CertType::Signing {
            warn!(workload = %claims.workload_identity, "signing certificate requested on /issue: refused");
            return error_response(
                IssueOutcome::CallerNotAuthorized,
                "a signing certificate is issued only to an internal caller, for itself",
            );
        }

        // ---- Step 1.5: who is asking ----
        //
        // The token says whose workload this is and nothing about whether it may ask for this. A caller
        // vouched for by the credentials agent's mTLS identity states the certificate itself; a caller
        // with only a token is limited to the bootstrap identities and what is written down for them.
        if let Some(detail) = self.caller_refusal(
            peer_identity,
            &claims.workload_identity,
            Some((&request.cert_type, &request.extra_sans)),
        ) {
            return error_response(IssueOutcome::CallerNotAuthorized, &detail);
        }

        // ---- Step 2: CSR parsing ----
        let parsed_csr = match csr::parse_csr(&request.csr_pem) {
            Ok(c) => c,
            Err(e) => return error_response(IssueOutcome::InvalidCsr, &e.to_string()),
        };

        // ---- Step 3: identity match ----
        //
        // The CSR's SAN must equal the workload identity claim from
        // the OIDC token. This is what prevents one agent's token
        // from being used to obtain a cert for another agent's
        // identity. The CSR module's `verify_identity` does the
        // actual byte-exact comparison.
        // First, we normalize the identity string, using this
        // issuer's own strategy and (if configured) scope claim --
        // both carried through on ValidatedClaims from oidc.rs, which
        // already knows which issuer matched.
        let dns_identity = match normalize_identity(
            &claims.workload_identity,
            claims.identity_scope.as_deref(),
            &claims.identity_normalization,
        ) {
            Ok(id) => id,
            Err(e) => return error_response(IssueOutcome::InvalidToken, &e.to_string()),
        };
        // The custodians' space is signed in only as a custodian's certificate, for the workload the token proves
        // (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian"): a workload named like one (a
        // service account may have dots in its name) is refused whatever it asks for.
        if cert_issuer_common::custody::is_reserved(&dns_identity) {
            warn!(identity = %dns_identity, "a token whose workload is named like a custodian: refused");
            return error_response(
                IssueOutcome::CallerNotAuthorized,
                "no workload's identity may be in the custodians' space",
            );
        }
        let workload_identity = dns_identity;
        let dns_identity = if request.cert_type == cert_issuer_common::CertType::Custody {
            cert_issuer_common::custody::custodian_identity(&workload_identity)
        } else {
            workload_identity.clone()
        };
        if let Err(e) = csr::verify_identity(&parsed_csr, &dns_identity) {
            return error_response(IssueOutcome::IdentityMismatch, &e.to_string());
        }
        if let Some(detail) = custody_names_refusal(&request.cert_type, &request.extra_sans) {
            warn!(identity = %dns_identity, "{detail}");
            return error_response(IssueOutcome::InvalidCsr, &detail);
        }

        // ---- Step 3.2: administrative capabilities are not requestable here ----
        //
        // Extra SANs on /issue are caller-supplied, a weaker claim than
        // the identity. A `c:` capability SAN asserts authority over the
        // Credentials Agent itself, so it may only be issued on
        // /internal/issue, where the caller is the allowlisted
        // credentials-agent.
        if let Some(san) = request
            .extra_sans
            .iter()
            .find(|san| san.trim().to_ascii_lowercase().starts_with("c:"))
        {
            warn!(identity = %dns_identity, san = %san, "capability SAN requested on /issue: refused");
            return error_response(
                IssueOutcome::InvalidCsr,
                &format!("capability SANs cannot be requested on /issue: {san}"),
            );
        }

        // ---- Step 3.3: the floor's topics and groups ----
        //
        // A bootstrap service signed on its token alone carries the topics and groups the catalog wrote into its
        // entry; the credentials agent states a vouched workload's own.
        let floor_names = match (&self.floor, self.vouched_for(peer_identity)) {
            (Some(floor), false) => {
                crate::floor::floor_names(floor, &claims.workload_identity, &request.cert_type)
            }
            _ => Vec::new(),
        };

        // ---- Step 3.4: the floor's state ----
        //
        // A bootstrap service on its token alone is signed when the floor state has its entry open; otherwise its
        // request is held, offered to the credentials agent, and signed once it vouches for it
        // (docs/architecture/core-restarts.md, "3 in detail").
        let gated = match self.gate_on_token(own, peer_identity, &claims, &dns_identity, &request) {
            Ok(gated) => gated,
            Err(response) => return response,
        };

        // ---- Steps 3.5-5: shared signing path ----
        //
        // Everything past identity verification is identical for
        // /issue and /internal/issue, so it lives in one place.
        let vouched = self.vouched_for(peer_identity) || matches!(gated, Gated::Vouched(_));
        let signed_cert_type = request.cert_type.clone();
        let response = self
            .sign_verified(
                &dns_identity,
                &parsed_csr,
                request.csr_pem,
                request.cert_type,
                request.extra_sans.into_iter().chain(floor_names).collect(),
                request.subject_dn_defaults,
                None,
                // For the record: its own, vouched for by the credentials agent, against the credentials agent's
                // successor ticket, or on the token alone.
                if own {
                    cert_issuer_common::record::IssuePath::Own
                } else if vouched {
                    cert_issuer_common::record::IssuePath::Vouched
                } else if matches!(gated, Gated::Ticketed) {
                    cert_issuer_common::record::IssuePath::Ticketed
                } else {
                    cert_issuer_common::record::IssuePath::TokenOnly
                },
                claims.instance_binding.clone(),
                // A custodian lives as long as its workload's certificates would.
                Some(&workload_identity),
                None,
            )
            .await;
        if let Some(gate) = &self.gate {
            match gated {
                Gated::Vouched(key) if response.status == 200 => gate.signed(&key),
                // A signing the ticket admitted that failed: its type may be asked for again.
                Gated::Ticketed if response.status != 200 => {
                    gate.give_back_ticket(&signed_cert_type)
                }
                _ => {}
            }
        }
        response
    }

    /// The signing tail shared by `handle` and `handle_internal_issue`:
    /// extra-SAN validation, Subject DN merge, lifetime selection, and
    /// the CA call. Callers must already have authenticated the request
    /// and verified that the CSR's SAN matches `dns_identity` -- this
    /// function assumes both and checks neither.
    #[allow(clippy::too_many_arguments)]
    async fn sign_verified(
        &self,
        dns_identity: &str,
        parsed_csr: &csr::ParsedCsr,
        csr_pem: String,
        cert_type: cert_issuer_common::CertType,
        extra_sans: Vec<String>,
        subject_dn_defaults: Option<cert_issuer_common::SubjectDnDefaults>,
        account_subject: Option<cert_issuer_common::AccountSubject>,
        path: cert_issuer_common::record::IssuePath,
        instance_binding: Option<String>,
        lifetime_of: Option<&str>,
        expires_by: Option<i64>,
    ) -> HandlerResponse {
        // TODO: pass this in so we get a cert valid for multiple SANs
        let all_sans = match csr::validate_extra_sans(dns_identity, &extra_sans) {
            Ok(all_sans) => all_sans,
            Err(e) => {
                error!("{e}");
                return error_response(IssueOutcome::InvalidCsr, &e.to_string());
            }
        };

        // ---- Step 3.4: account subject ----
        //
        // Authoritative username and email, from the internal caller. A
        // value that fails validation is an internal-caller bug, not a
        // CSR problem, so it's an InternalError.
        if let Some(subject) = &account_subject
            && let Err(e) = validate_account_subject(subject)
        {
            error!("account_subject failed validation: {e}");
            return error_response(IssueOutcome::InternalError, &e);
        }

        // ---- Step 3.5: subject DN merge ----
        //
        // Per field: the CSR's own value wins if present (the user
        // spoke for themselves); otherwise fall back to the admin's
        // provisioned default, if any; otherwise the field is simply
        // absent from the issued cert. A default that fails
        // validation is an internal-caller bug (credentials-agent,
        // not the CSR/attacker), so it's reported as InternalError,
        // not InvalidCsr. Username and email are the exception: they
        // come only from `account_subject`, never from the CSR.
        let subject_dn = match merge_subject_dn(
            &parsed_csr.subject,
            subject_dn_defaults.as_ref(),
            account_subject.as_ref(),
        ) {
            Ok(s) => s,
            Err(e) => {
                error!("subject_dn_defaults failed validation: {e}");
                return error_response(IssueOutcome::InternalError, &e.to_string());
            }
        };

        // ---- Step 4: session ID ----
        //
        // Fresh UUID per request. Used downstream for audit
        // correlation between issuance events and certificate use.
        let session_id = generate_session_id();

        // ---- Step 5: CA call ----
        //
        // We pass only the CSR PEM and the requested lifetime. The
        // CA does not need the expected identity separately — the SAN
        // is embedded in the CSR, and the handler has already verified
        // it matches the token. The CA's job is to sign; the
        // verification work is done.
        let lifetime = self
            .identity_lifetime_overrides
            .get(lifetime_of.unwrap_or(dns_identity))
            .copied()
            .unwrap_or(if account_subject.is_some() {
                // An account certificate: the person's own identity, which
                // lives as long as `user_lifetime` says, whatever CertType
                // the credentials agent asked for.
                self.user_lifetime
            } else {
                match cert_type {
                    cert_issuer_common::CertType::Server => self.server_lifetime,
                    cert_issuer_common::CertType::Client
                    | cert_issuer_common::CertType::Custody => self.default_lifetime,
                    // Not short-lived like Client: a service's durable identity, which senders encrypt to long after
                    // the pod that asked has gone, and which is never issued twice for an identity. What protects it
                    // is to be revocation, once built, not a tight expiry; the lifetime is to be revisited then.
                    cert_issuer_common::CertType::Service => self.service_lifetime,
                    // The same: the agent's own, renewed by it.
                    cert_issuer_common::CertType::Signing => self.server_lifetime,
                }
            });

        // No certificate outlives the end it was given (a custodian's), and none is signed once that is past.
        let lifetime = match expires_by {
            None => lifetime,
            Some(end) => {
                let left = end - time::OffsetDateTime::now_utc().unix_timestamp();
                if left <= 0 {
                    return error_response(
                        IssueOutcome::CallerNotAuthorized,
                        "the certificate would end before it began",
                    );
                }
                lifetime.min(std::time::Duration::from_secs(left as u64))
            }
        };

        let ca_request = CaIssueRequest {
            csr_pem,
            sans: all_sans,
            lifetime,
            cert_type,
            subject_dn,
            path,
            instance_binding,
        };

        let issued = match self.ca.issue(ca_request).await {
            Ok(c) => c,
            Err(e) => {
                // CaError::Malformed means the CSR passed our own
                // parser but rcgen's internal parser rejected it —
                // a structural issue in the CSR, surfaces as 400.
                // CaError::Internal means rcgen's signing step
                // failed, which is a bug or corrupted key — not
                // transient, surfaces as 500.
                let (outcome, detail) = match e {
                    CaError::Malformed(s) => {
                        (IssueOutcome::InvalidCsr, format!("CA rejected CSR: {s}"))
                    }
                    CaError::SigningFailed(s) => (
                        IssueOutcome::InternalError,
                        format!("CA signing error: {s}"),
                    ),
                };
                warn!(session_id = %session_id, detail = %detail, "CA failure");
                return error_response(outcome, &detail);
            }
        };

        // ---- Success ----
        let expires_at = issued.not_after;
        HandlerResponse {
            status: 200,
            body: HandlerBody::Success(IssueResponse {
                certificate_pem: issued.certificate_pem,
                ca_chain_pem: issued.ca_chain_pem,
                serial_number: issued.serial_number,
                session_id,
                expires_at,
            }),
        }
    }

    /// Process an `/internal/issue` request. There is no bearer token:
    /// the caller has already done its own authorization elsewhere
    /// (credentials-agent verifies a bootstrap token or an account
    /// claim) and asserts the identity to sign for directly. What this
    /// function verifies is who is asking -- the verified peer identity
    /// of the mTLS connection, passed in by the transport layer --
    /// against `internal_callers`, and then that the CSR's SAN matches
    /// the asserted identity, exactly as `/issue` does against a token.
    // skip_all for the same reason as `handle`; the verified peer identity is
    // the one safe, useful value, so it is recorded explicitly.
    #[instrument(skip_all, fields(peer_identity = ?peer_identity))]
    pub async fn handle_internal_issue(
        &self,
        peer_identity: Option<&str>,
        request: InternalIssueRequest,
    ) -> HandlerResponse {
        // ---- Step 1: caller authorization ----
        //
        // No client certificate, or one whose identity isn't on the
        // allowlist, is refused before the CSR is even parsed. The
        // detail deliberately doesn't say which of the two it was, or
        // what the allowlist contains.
        let caller = match peer_identity {
            Some(peer) if self.internal_callers.iter().any(|c| c == peer) => peer,
            other => {
                warn!(peer_identity = ?other, "internal issuance refused: caller not on allowlist");
                return error_response(
                    IssueOutcome::CallerNotAuthorized,
                    "caller is not authorized for internal issuance",
                );
            }
        };

        // ---- Step 1.5: a signing certificate is the caller's own ----
        //
        // The caller asserts the identity of everything else it asks for here; a key to sign with it may have
        // only for itself.
        if request.cert_type == cert_issuer_common::CertType::Signing && request.identity != caller
        {
            warn!(
                peer_identity = %caller,
                identity = %request.identity,
                "internal issuance refused: a signing certificate for another identity"
            );
            return error_response(
                IssueOutcome::CallerNotAuthorized,
                "a signing certificate is issued only for the caller's own identity",
            );
        }

        // ---- Step 1.6: no custodian here ----
        //
        // A custodian is what a pod's token buys, and only a token (docs/architecture/key-custody.md, "Part A's
        // bootstrap: the custodian"): the internal caller signs nothing in the custodians' space, and no custody name.
        if request.cert_type == cert_issuer_common::CertType::Custody
            || cert_issuer_common::custody::is_reserved(&request.identity)
        {
            warn!(peer_identity = %caller, identity = %request.identity, "internal issuance refused: the custodians' space");
            return error_response(
                IssueOutcome::CallerNotAuthorized,
                "a custodian's certificate is signed only on its pod's token",
            );
        }
        if let Some(detail) = custody_names_refusal(&request.cert_type, &request.extra_sans) {
            return error_response(IssueOutcome::InvalidCsr, &detail);
        }

        // ---- Step 1.7: a server certificate's names are its workload's ----
        //
        // As on /issue for a vouched request: the floor's list of names for the workload, whoever asks.
        if request.cert_type == cert_issuer_common::CertType::Server
            && let Some(floor) = &self.floor
        {
            let workload = cert_issuer_common::identity::kubernetes_subject_of(&request.identity);
            if let Err(refusal) = crate::floor::admit_vouched(
                floor,
                workload.as_deref().unwrap_or(&request.identity),
                &request.cert_type,
                &request.extra_sans,
            ) {
                warn!(identity = %request.identity, "internal issuance refused: {refusal}");
                return error_response(IssueOutcome::CallerNotAuthorized, &refusal.to_string());
            }
        }

        // ---- Step 2: CSR parsing ----
        let parsed_csr = match csr::parse_csr(&request.csr_pem) {
            Ok(c) => c,
            Err(e) => return error_response(IssueOutcome::InvalidCsr, &e.to_string()),
        };

        // ---- Step 3: identity match ----
        //
        // Same byte-exact check /issue does, against the identity the
        // caller asserted instead of one derived from a token.
        if request.identity.is_empty() {
            return error_response(IssueOutcome::InvalidCsr, "identity must not be empty");
        }
        if let Err(e) = csr::verify_identity(&parsed_csr, &request.identity) {
            return error_response(IssueOutcome::IdentityMismatch, &e.to_string());
        }

        self.sign_verified(
            &request.identity,
            &parsed_csr,
            request.csr_pem,
            request.cert_type,
            request.extra_sans,
            request.subject_dn_defaults,
            request.account_subject,
            cert_issuer_common::record::IssuePath::Internal,
            request.instance_binding,
            None,
            request.expires_by,
        )
        .await
    }

    /// Process a `/validate` request -- OIDC validation only, no CSR,
    /// no signing. Exists specifically for credentials-agent's
    /// two-call issuance flow: it needs the real, validated identity
    /// (for a grant lookup and the renewal-prevention check) *before*
    /// it has anything to embed into a CSR's SAN set, since permitted
    /// SANs come from the grant, not known upfront.
    #[instrument(skip_all)]
    pub async fn handle_validate(
        &self,
        peer_identity: Option<&str>,
        bearer_token: &str,
        request: ValidateRequest,
    ) -> ValidateHandlerResponse {
        let claims = match self.validator.validate(bearer_token).await {
            Ok(claims) => claims,
            Err(e) => {
                return ValidateHandlerResponse {
                    status: status_for_outcome(map_validation_error(&e)),
                    body: ValidateHandlerBody::Error(IssueError {
                        outcome: map_validation_error(&e),
                        detail: e.to_string(),
                    }),
                };
            }
        };

        // Who is asking: the same rule as /issue, without a request to inspect.
        if let Some(detail) = self.caller_refusal(peer_identity, &claims.workload_identity, None) {
            return ValidateHandlerResponse {
                status: status_for_outcome(IssueOutcome::CallerNotAuthorized),
                body: ValidateHandlerBody::Error(IssueError {
                    outcome: IssueOutcome::CallerNotAuthorized,
                    detail,
                }),
            };
        }

        // Computed via the exact same normalize_identity call /issue
        // itself uses -- a client building a CSR from this value is
        // guaranteed to match what /issue will check, never a second,
        // independent computation that could drift. Only computed
        // when actually asked for.
        let normalized_identity = if request.include_normalized_identity {
            match normalize_identity(
                &claims.workload_identity,
                claims.identity_scope.as_deref(),
                &claims.identity_normalization,
            ) {
                Ok(id) => Some(id),
                Err(e) => {
                    return ValidateHandlerResponse {
                        status: status_for_outcome(IssueOutcome::InvalidToken),
                        body: ValidateHandlerBody::Error(IssueError {
                            outcome: IssueOutcome::InvalidToken,
                            detail: e.to_string(),
                        }),
                    };
                }
            }
        } else {
            None
        };

        ValidateHandlerResponse {
            status: 200,
            body: ValidateHandlerBody::Success(ValidateResponse {
                workload_identity: claims.workload_identity,
                instance_binding: claims.instance_binding,
                jti: claims.jti,
                normalized_identity,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a `ValidationError` to the corresponding wire-level outcome.
///
/// The mapping is the entire point of having a separate
/// `IssueOutcome` enum — operators and clients see a stable wire
/// contract even when the internal validation error variants change.
fn map_validation_error(e: &ValidationError) -> IssueOutcome {
    match e {
        ValidationError::InvalidAudience => IssueOutcome::InvalidAudience,
        ValidationError::InvalidIssuer(_) => IssueOutcome::InvalidToken,
        ValidationError::ForbiddenAlgorithm(_) => IssueOutcome::InvalidToken,
        ValidationError::Expired => IssueOutcome::InvalidToken,
        ValidationError::MissingClaim(_) => IssueOutcome::InvalidToken,
        ValidationError::JwksUnavailable(_) => IssueOutcome::InternalError,
        ValidationError::InvalidToken => IssueOutcome::InvalidToken,
    }
}

/// Plain, ready-to-push Subject DN field values -- every field
/// already validated, either by the CSR's own extraction
/// (csr::extract_subject_fields) or here, against the same newtypes,
/// for an admin-supplied default. `username` and `email` are the
/// authoritative account attributes (`uid`, `mail`): they are never taken
/// from the CSR.
#[derive(Debug, Clone, Default)]
pub struct MergedSubjectDn {
    pub given_name: Option<String>,
    pub surname: Option<String>,
    pub title: Option<String>,
    pub telephone_number: Option<String>,
    pub username: Option<String>,
    pub email: Option<String>,
}

/// Merges the CSR's own Subject DN fields with an admin's provisioned
/// defaults, per field: the CSR's value wins when present, the
/// default fills the gap otherwise. See MergedSubjectDn's own doc
/// comment for why this exists as a distinct step, and Step 3.5's
/// own comment above for the fail-closed behavior on a bad default.
/// The authoritative account subject, if any, is copied through as is
/// (it was validated in Step 3.4).
fn merge_subject_dn(
    csr_fields: &crate::csr::SubjectFields,
    defaults: Option<&cert_issuer_common::SubjectDnDefaults>,
    account_subject: Option<&cert_issuer_common::AccountSubject>,
) -> Result<MergedSubjectDn, crate::csr::CsrError> {
    use crate::csr::{JobTitle, PersonalName, PhoneNumber};
    let d = defaults.cloned().unwrap_or_default();

    let given_name = match &csr_fields.given_name {
        Some(v) => Some(v.as_str().to_string()),
        None => d
            .given_name
            .as_deref()
            .map(PersonalName::parse)
            .transpose()?
            .map(|v| v.as_str().to_string()),
    };
    let surname = match &csr_fields.surname {
        Some(v) => Some(v.as_str().to_string()),
        None => d
            .surname
            .as_deref()
            .map(PersonalName::parse)
            .transpose()?
            .map(|v| v.as_str().to_string()),
    };
    let title = match &csr_fields.title {
        Some(v) => Some(v.as_str().to_string()),
        None => d
            .title
            .as_deref()
            .map(JobTitle::parse)
            .transpose()?
            .map(|v| v.as_str().to_string()),
    };
    let telephone_number = match &csr_fields.telephone_number {
        Some(v) => Some(v.as_str().to_string()),
        None => d
            .telephone_number
            .as_deref()
            .map(PhoneNumber::parse)
            .transpose()?
            .map(|v| v.as_str().to_string()),
    };

    Ok(MergedSubjectDn {
        given_name,
        surname,
        title,
        telephone_number,
        username: account_subject.map(|s| s.username.clone()),
        email: account_subject.map(|s| s.email.clone()),
    })
}

/// Checks the authoritative account subject before it goes into a
/// certificate. `mail` is an IA5String (RFC 4524), so the email must be
/// ASCII. Returned as a plain message: any failure here is a bug in the
/// internal caller, reported as an InternalError, not a CSR problem.
fn validate_account_subject(subject: &cert_issuer_common::AccountSubject) -> Result<(), String> {
    const MAX_LEN: usize = 256;
    for (field, value) in [("username", &subject.username), ("email", &subject.email)] {
        if value.is_empty() {
            return Err(format!("{field} must not be empty"));
        }
        if value.len() > MAX_LEN {
            return Err(format!("{field} is longer than {MAX_LEN} bytes"));
        }
        if value.chars().any(|c| c.is_control()) {
            return Err(format!("{field} contains a control character"));
        }
    }
    let email = &subject.email;
    if !email.is_ascii()
        || email.chars().any(char::is_whitespace)
        || email.matches('@').count() != 1
        || email.starts_with('@')
        || email.ends_with('@')
    {
        return Err(
            "email must be ASCII, without whitespace, with exactly one interior '@'".to_string(),
        );
    }
    Ok(())
}

/// Why a request's extra names may not go with its type: a custody name is a custodian's certificate's alone, and a
/// custodian's certificate carries custody names and nothing else (docs/architecture/key-custody.md, "Part A's
/// bootstrap: the custodian").
fn custody_names_refusal(
    cert_type: &cert_issuer_common::CertType,
    extra_sans: &[String],
) -> Option<String> {
    use cert_issuer_common::custody::{CustodyPermission, names_custody};
    if cert_type == &cert_issuer_common::CertType::Custody {
        extra_sans
            .iter()
            .find(|name| CustodyPermission::from_san_string(name).is_err())
            .map(|name| {
                format!("a custodian's certificate carries custody names only, not `{name}`")
            })
    } else {
        extra_sans
            .iter()
            .find(|name| names_custody(name))
            .map(|name| format!("a custody name is a custodian's certificate's alone: `{name}`"))
    }
}

fn error_response(outcome: IssueOutcome, detail: &str) -> HandlerResponse {
    HandlerResponse {
        status: status_for_outcome(outcome),
        body: HandlerBody::Error(IssueError {
            outcome,
            detail: detail.to_string(),
        }),
    }
}

/// Generate a fresh session ID for audit correlation.
///
/// We use UUID v4 for human-recognizable formatting (it's what
/// operators expect to see in logs). The collision probability is
/// negligible at any cert issuance rate we'll hit.
fn generate_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Status code mapping for outcomes. Pinned in code so tests can
/// assert the exact mapping without reading the handler.
pub fn status_for_outcome(outcome: IssueOutcome) -> u16 {
    match outcome {
        IssueOutcome::Success => 200,
        IssueOutcome::InvalidToken => 401,
        IssueOutcome::InvalidAudience => 401,
        IssueOutcome::IdentityMismatch => 403,
        IssueOutcome::InvalidCsr => 400,
        IssueOutcome::CaUnavailable => 503,
        IssueOutcome::InternalError => 500,
        // /internal/issue only -- same 403 as IdentityMismatch, since
        // both mean "the caller isn't who they'd need to be for this
        // request to proceed," just checked via different mechanisms
        // (mTLS peer identity here, token-derived SAN there).
        IssueOutcome::CallerNotAuthorized => 403,
        // Held, not refused: ask again shortly.
        IssueOutcome::Held => 202,
    }
}
