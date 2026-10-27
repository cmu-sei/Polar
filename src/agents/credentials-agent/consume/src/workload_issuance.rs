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

//! A workload's request for its pod-instance certificates before it has any certificate, with its token sealed to
//! an ephemeral session (credentials-agent-common's `workload_issuance`). The session ends here: the observer
//! relays the sealed bytes and never holds the token.
//!
//! Nothing about issuance is decided here. Once the token is unsealed the request is the same request the
//! unsealed route makes, and `IssuanceHandler` applies every rule it has: the credential's kind, the renewal rule
//! per type, the grant, the server names, cert-issuer. This only opens sessions, signs their offers, and opens the
//! seal.
//!
//! Each offer is signed with a key kept for nothing else, which cert-issuer certifies as a signing certificate for
//! this agent's own identity (credentials-agent-common's `session_offer`; docs/architecture/core-restarts.md, step
//! 1c). The key is made here, in memory, and replaced at half its certificate's life; until there is a certificate
//! valid now, a session request is told to try again.

use crate::issuance::IssuanceHandler;
use crate::sessions::{MAX_OPEN_SESSIONS, SESSION_TTL, Sessions};
use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
use cert_issuer_common::{CertType, InternalIssueRequest};
use credentials_agent_common::issuance::{IssuanceRefusalReason, IssuanceReply, IssuanceRequest};
use credentials_agent_common::session_offer::OfferSigner;
use credentials_agent_common::workload_issuance::{WorkloadIssuanceReply, WorkloadIssuanceRequest};
use std::time::{Duration, Instant};
use tracing::{debug, error, warn};

/// How long after a failed try for a signing certificate the next one is made, and the least time before any
/// renewal, so a certificate that is already past its half-life is not asked for in a loop.
pub const SIGNER_RETRY: Duration = Duration::from_secs(5);

/// How long from `now` until a signing certificate valid from `not_before` to `not_after` (all Unix seconds) is
/// replaced: at half its life, and never sooner than [`SIGNER_RETRY`]. One that is never valid, or no longer, is
/// replaced as soon as that.
pub fn renewal_delay(not_before: i64, not_after: i64, now: i64) -> Duration {
    if not_after <= not_before || not_after <= now {
        return SIGNER_RETRY;
    }
    let half_life = not_before.saturating_add(not_after.saturating_sub(not_before) / 2);
    let wait = u64::try_from(half_life.saturating_sub(now)).unwrap_or(0);
    Duration::from_secs(wait).max(SIGNER_RETRY)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub struct WorkloadIssuanceHandler {
    issuance: IssuanceHandler,
    sessions: Sessions,
    /// This agent's identity as cert-issuer reads it off the mTLS connection: what its signing certificate names.
    own_identity: String,
    /// What session offers are signed with; none until cert-issuer has issued a signing certificate.
    signer: Option<OfferSigner>,
}

impl WorkloadIssuanceHandler {
    pub fn new(issuance: IssuanceHandler, own_identity: String) -> Self {
        Self::with_sessions(
            issuance,
            Sessions::new(SESSION_TTL, MAX_OPEN_SESSIONS),
            own_identity,
        )
    }

    pub fn with_sessions(
        issuance: IssuanceHandler,
        sessions: Sessions,
        own_identity: String,
    ) -> Self {
        Self {
            issuance,
            sessions,
            own_identity,
            signer: None,
        }
    }

    /// Signs with `signer` from the start, as if it had just been renewed.
    pub fn with_signer(mut self, signer: OfferSigner) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Makes a new signing keypair and has cert-issuer certify it for this agent's own identity, on
    /// `/internal/issue`, then signs with it. The signer it replaces is kept until then, so a failure leaves offers
    /// signed as before. `Ok` is how long until the next renewal.
    pub async fn renew_signer(&mut self) -> Result<Duration, String> {
        let csr = generate_csr(&self.own_identity, &KeyAlgorithm::Ed25519, &[])
            .map_err(|e| format!("cannot make a signing key: {e}"))?;
        let issued = self
            .issuance
            .cert_issuer
            .issue_internal(InternalIssueRequest {
                csr_pem: csr.csr_pem,
                identity: self.own_identity.clone(),
                cert_type: CertType::Signing,
                extra_sans: vec![],
                subject_dn_defaults: None,
                account_subject: None,
            })
            .await
            .map_err(|e| format!("cert-issuer did not issue a signing certificate: {e}"))?;
        // The certificate alone: the CA that signed it is the one every workload pins.
        let signer = OfferSigner::new(vec![issued.certificate_pem], csr.private_key_pem)
            .map_err(|e| format!("cert-issuer's signing certificate cannot be used: {e}"))?;
        let delay = renewal_delay(signer.not_before(), signer.not_after(), unix_now());
        self.signer = Some(signer);
        Ok(delay)
    }

    pub async fn handle(
        &mut self,
        request: WorkloadIssuanceRequest,
        now: Instant,
    ) -> WorkloadIssuanceReply {
        match request {
            WorkloadIssuanceRequest::OpenSession => {
                let Some(signer) = self.signer.as_ref().filter(|s| s.valid_at(unix_now())) else {
                    warn!("a session was refused: there is no signing certificate valid now");
                    return WorkloadIssuanceReply::Busy;
                };
                let Ok(opened) = self.sessions.open(now) else {
                    warn!("a session was refused: too many are open");
                    return WorkloadIssuanceReply::Busy;
                };
                match signer.sign(
                    opened.session_id.clone(),
                    opened.public_key,
                    opened.valid_for_secs,
                ) {
                    Ok(offer) => {
                        debug!(open = self.sessions.len(), "a session was opened");
                        WorkloadIssuanceReply::Offer(offer)
                    }
                    Err(e) => {
                        // Not offered, so not kept.
                        self.sessions.take(&opened.session_id, now);
                        error!("a session offer could not be signed: {e}");
                        WorkloadIssuanceReply::Busy
                    }
                }
            }
            WorkloadIssuanceRequest::Issue(sealed) => {
                // The session is used up here whatever comes of the seal: one try per session.
                let Some(session) = self.sessions.take(&sealed.session_id, now) else {
                    return WorkloadIssuanceReply::UnknownSession;
                };
                let credential = match session
                    .unseal(&sealed.sealed_credential)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
                {
                    Some(credential) => credential,
                    None => {
                        // Not sealed to this session, or changed on the way, or not text. Said without saying
                        // which, and without the payload.
                        warn!("a sealed credential did not open");
                        return WorkloadIssuanceReply::Issuance(IssuanceReply::Refused {
                            reason: IssuanceRefusalReason::TokenInvalid,
                        });
                    }
                };
                let reply = self
                    .issuance
                    .handle(IssuanceRequest {
                        csr_pem: sealed.csr_pem,
                        credential,
                        cert_type: sealed.cert_type,
                        names: sealed.names,
                    })
                    .await;
                WorkloadIssuanceReply::Issuance(reply)
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::issuance::tests::{
        CASSINI_NAME, K8S, handler_for, kubernetes_pod, make_token, response_with_serial,
        spawn_test_ledger,
    };
    use credentials_agent_common::ephemeral_exchange::EphemeralEncryptedPayload;
    use credentials_agent_common::issuance::WorkloadCertType;
    use credentials_agent_common::workload_issuance::{SealedIssuanceRequest, SessionOffer};
    use std::time::Duration;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(crate) const OWN_IDENTITY: &str = "credentials-agent.polar.serviceaccount.cluster.local";

    /// What cert-issuer does with a CSR on `/internal/issue` for a signing certificate: signs it under `ca`, for
    /// `lifetime` from a minute ago, digital signature and document signing only.
    pub(crate) fn certify(
        ca: &polar_signing::testing::Authority,
        csr_pem: &str,
        lifetime: time::Duration,
    ) -> String {
        let mut csr = rcgen::CertificateSigningRequestParams::from_pem(csr_pem).unwrap();
        let now = time::OffsetDateTime::now_utc();
        csr.params.not_before = now - time::Duration::minutes(1);
        csr.params.not_after = now + lifetime;
        csr.params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        csr.params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::Other(
            cert_issuer_common::OID_DOCUMENT_SIGNING.to_vec(),
        )];
        csr.signed_by(&ca.issuer).unwrap().pem()
    }

    /// A signer for this agent, its certificate from `ca`, valid for `lifetime` from a minute ago.
    pub(crate) fn a_signer(
        ca: &polar_signing::testing::Authority,
        lifetime: time::Duration,
    ) -> OfferSigner {
        let csr = generate_csr(OWN_IDENTITY, &KeyAlgorithm::Ed25519, &[]).unwrap();
        OfferSigner::new(
            vec![certify(ca, &csr.csr_pem, lifetime)],
            csr.private_key_pem,
        )
        .unwrap()
    }

    fn the_ca() -> &'static polar_signing::testing::Authority {
        static CA: std::sync::OnceLock<polar_signing::testing::Authority> =
            std::sync::OnceLock::new();
        CA.get_or_init(|| polar_signing::testing::root("Polar Internal CA"))
    }

    async fn handler(server: &MockServer) -> WorkloadIssuanceHandler {
        unsigned(server)
            .await
            .with_signer(a_signer(the_ca(), time::Duration::hours(1)))
    }

    async fn unsigned(server: &MockServer) -> WorkloadIssuanceHandler {
        WorkloadIssuanceHandler::new(
            handler_for(server, spawn_test_ledger().await).await,
            OWN_IDENTITY.to_string(),
        )
    }

    pub(crate) fn check() -> credentials_agent_common::session_offer::OfferCheck {
        credentials_agent_common::session_offer::OfferCheck::new(
            &the_ca().pem,
            OWN_IDENTITY.to_string(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn an_offer_is_signed_as_the_agent_and_a_workload_believes_it() {
        let server = MockServer::start().await;
        let mut handler = handler(&server).await;
        let offer = opened(&mut handler, Instant::now()).await;
        assert_eq!(check().check(&offer, unix_now()), Ok(()));
    }

    #[tokio::test]
    async fn without_a_signing_certificate_valid_now_no_session_is_opened() {
        let server = MockServer::start().await;
        let mut none = unsigned(&server).await;
        let mut expired = unsigned(&server)
            .await
            .with_signer(a_signer(the_ca(), time::Duration::seconds(-30)));
        for handler in [&mut none, &mut expired] {
            let reply = handler
                .handle(WorkloadIssuanceRequest::OpenSession, Instant::now())
                .await;
            assert!(matches!(reply, WorkloadIssuanceReply::Busy), "{reply:?}");
            assert!(
                handler.sessions.is_empty(),
                "nothing opened that cannot be offered"
            );
        }
    }

    /// A cert-issuer that signs a signing certificate on `/internal/issue` for this agent, as the real one does.
    pub(crate) struct CertifiesSigning;

    impl wiremock::Respond for CertifiesSigning {
        fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
            let asked: cert_issuer_common::InternalIssueRequest = request.body_json().unwrap();
            ResponseTemplate::new(200).set_body_json(cert_issuer_common::IssueResponse {
                certificate_pem: certify(the_ca(), &asked.csr_pem, time::Duration::hours(1)),
                ca_chain_pem: the_ca().pem.clone(),
                serial_number: "signing-1".to_string(),
                session_id: "00000000-0000-0000-0000-000000000001".to_string(),
                expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
            })
        }
    }

    #[tokio::test]
    async fn renewing_asks_cert_issuer_for_a_signing_certificate_for_the_agent_itself_and_signs_with_it()
     {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .and(body_partial_json(serde_json::json!({
                "identity": OWN_IDENTITY,
                "cert_type": "SIGNING",
                "extra_sans": [],
            })))
            .respond_with(CertifiesSigning)
            .expect(2)
            .mount(&server)
            .await;
        let mut handler = unsigned(&server).await;

        let delay = handler.renew_signer().await.unwrap();
        // Valid from a minute ago for an hour from now: 61 minutes, renewed at half of them, 29.5 minutes on.
        assert!(
            (Duration::from_secs(1770 - 5)..=Duration::from_secs(1770 + 5)).contains(&delay),
            "{delay:?}"
        );
        let first = opened(&mut handler, Instant::now()).await;
        assert_eq!(check().check(&first, unix_now()), Ok(()));

        handler.renew_signer().await.unwrap();
        let second = opened(&mut handler, Instant::now()).await;
        assert_eq!(check().check(&second, unix_now()), Ok(()));
        assert_ne!(
            first.signature.certificate_chain, second.signature.certificate_chain,
            "a new key and certificate each time"
        );
    }

    #[tokio::test]
    async fn a_failed_renewal_keeps_the_signer_there_was_and_without_one_offers_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .respond_with(ResponseTemplate::new(503).set_body_string("down"))
            .mount(&server)
            .await;

        let mut signed = handler(&server).await;
        assert!(signed.renew_signer().await.is_err());
        let offer = opened(&mut signed, Instant::now()).await;
        assert_eq!(check().check(&offer, unix_now()), Ok(()));

        let mut unsigned = unsigned(&server).await;
        assert!(unsigned.renew_signer().await.is_err());
        assert!(matches!(
            unsigned
                .handle(WorkloadIssuanceRequest::OpenSession, Instant::now())
                .await,
            WorkloadIssuanceReply::Busy
        ));
    }

    #[test]
    fn a_signing_certificate_is_renewed_at_half_its_life_and_never_in_a_loop() {
        // A fresh hour's certificate: half an hour.
        assert_eq!(renewal_delay(1000, 4600, 1000), Duration::from_secs(1800));
        // Twenty minutes in: ten more.
        assert_eq!(renewal_delay(1000, 4600, 2200), Duration::from_secs(600));
        // Past its half-life, or expired, or nonsense: the retry interval, not zero.
        assert_eq!(renewal_delay(1000, 4600, 3000), SIGNER_RETRY);
        assert_eq!(renewal_delay(1000, 4600, 9000), SIGNER_RETRY);
        assert_eq!(renewal_delay(4600, 1000, 1000), SIGNER_RETRY);
        assert_eq!(renewal_delay(1000, 1000, 0), SIGNER_RETRY);
        assert_eq!(renewal_delay(i64::MIN, i64::MAX, 0), SIGNER_RETRY);
    }

    proptest::proptest! {
        #[test]
        fn renewal_waits_for_half_the_life_and_at_least_the_retry_interval(
            not_before in -1_000_000i64..1_000_000,
            life in -1_000i64..1_000_000,
            elapsed in -1_000_000i64..2_000_000,
        ) {
            let not_after = not_before + life;
            let now = not_before + elapsed;
            let delay = renewal_delay(not_before, not_after, now).as_secs() as i64;
            let retry = SIGNER_RETRY.as_secs() as i64;
            // A certificate that is not valid now or later is replaced as soon as may be.
            if life <= 0 || now >= not_after {
                proptest::prop_assert_eq!(delay, retry);
            }
            let half_life = not_before + life / 2;
            // Never sooner than the retry interval: no loop, whatever the certificate says.
            proptest::prop_assert!(delay >= retry);
            // Never later than half its life, unless that is within the retry interval.
            proptest::prop_assert!(now + delay <= half_life.max(now + retry));
            // And for a certificate valid now or later, with time to spare, exactly at half its life.
            if life > 0 && now < not_after && half_life - now >= retry {
                proptest::prop_assert_eq!(now + delay, half_life);
            }
        }
    }

    async fn opened(handler: &mut WorkloadIssuanceHandler, now: Instant) -> SessionOffer {
        match handler
            .handle(WorkloadIssuanceRequest::OpenSession, now)
            .await
        {
            WorkloadIssuanceReply::Offer(offer) => offer,
            other => panic!("expected an offer, got {other:?}"),
        }
    }

    /// What a workload does: seal the token to the offer and name the session.
    fn sealed(
        offer: &SessionOffer,
        token: &str,
        cert_type: WorkloadCertType,
        names: &[&str],
    ) -> SealedIssuanceRequest {
        SealedIssuanceRequest {
            session_id: offer.session_id.clone(),
            csr_pem: "fake-csr".to_string(),
            cert_type,
            names: names.iter().map(|s| s.to_string()).collect(),
            sealed_credential: EphemeralEncryptedPayload::encrypt(
                token.as_bytes(),
                &offer.public_key,
            )
            .unwrap(),
        }
    }

    fn issued(reply: &WorkloadIssuanceReply) -> bool {
        matches!(
            reply,
            WorkloadIssuanceReply::Issuance(IssuanceReply::Issued { .. })
        )
    }

    #[tokio::test]
    async fn a_token_sealed_to_an_opened_session_reaches_cert_issuer_as_that_token() {
        let server = MockServer::start().await;
        let token = make_token(K8S);
        // cert-issuer must be asked with the real token, unsealed.
        Mock::given(method("POST"))
            .and(path("/validate"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:git-server".to_string(),
                    instance_binding: Some("pod-1".to_string()),
                    jti: None,
                    normalized_identity: Some(
                        "git-server.polar.serviceaccount.cluster.local".to_string(),
                    ),
                },
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response_with_serial("client-1")),
            )
            .mount(&server)
            .await;
        let mut handler = handler(&server).await;
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;

        let reply = handler
            .handle(
                WorkloadIssuanceRequest::Issue(sealed(
                    &offer,
                    &token,
                    WorkloadCertType::Client,
                    &[],
                )),
                now,
            )
            .await;
        assert!(issued(&reply), "{reply:?}");
    }

    #[tokio::test]
    async fn the_type_and_names_a_sealed_request_makes_are_carried_through_to_cert_issuer() {
        let server = MockServer::start().await;
        kubernetes_pod(&server, "pod-1").await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(body_partial_json(serde_json::json!({
                "cert_type": "SERVER",
                "extra_sans": [CASSINI_NAME]
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response_with_serial("server-1")),
            )
            .mount(&server)
            .await;
        let mut handler = handler(&server).await;
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;
        let reply = handler
            .handle(
                WorkloadIssuanceRequest::Issue(sealed(
                    &offer,
                    &make_token(K8S),
                    WorkloadCertType::Server,
                    &[CASSINI_NAME],
                )),
                now,
            )
            .await;
        assert!(issued(&reply), "{reply:?}");
    }

    #[tokio::test]
    async fn the_rules_of_issuance_still_apply_to_a_sealed_request() {
        // A person's credential asking for a server certificate is refused by IssuanceHandler, not here.
        let server = MockServer::start().await;
        let mut handler = handler(&server).await;
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;
        let reply = handler
            .handle(
                WorkloadIssuanceRequest::Issue(sealed(
                    &offer,
                    &make_token("https://keycloak.example/realms/polar"),
                    WorkloadCertType::Server,
                    &[CASSINI_NAME],
                )),
                now,
            )
            .await;
        assert!(matches!(
            reply,
            WorkloadIssuanceReply::Issuance(IssuanceReply::Refused {
                reason: IssuanceRefusalReason::RequestNotPermitted
            })
        ));
    }

    #[tokio::test]
    async fn a_session_is_used_once_and_a_replayed_request_finds_it_gone() {
        let server = MockServer::start().await;
        let mut handler = handler(&server).await;
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;
        let request = sealed(&offer, &make_token(K8S), WorkloadCertType::Client, &[]);
        handler
            .handle(WorkloadIssuanceRequest::Issue(request.clone()), now)
            .await;
        let again = handler
            .handle(WorkloadIssuanceRequest::Issue(request), now)
            .await;
        assert!(
            matches!(again, WorkloadIssuanceReply::UnknownSession),
            "{again:?}"
        );
    }

    #[tokio::test]
    async fn an_expired_or_unknown_session_is_unknown_and_nothing_is_asked_of_cert_issuer() {
        let server = MockServer::start().await;
        let mut handler = handler(&server).await;
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;
        let late = now + SESSION_TTL;
        let reply = handler
            .handle(
                WorkloadIssuanceRequest::Issue(sealed(
                    &offer,
                    &make_token(K8S),
                    WorkloadCertType::Client,
                    &[],
                )),
                late,
            )
            .await;
        assert!(matches!(reply, WorkloadIssuanceReply::UnknownSession));
        let stranger = SealedIssuanceRequest {
            session_id: "never-opened".to_string(),
            ..sealed(&offer, &make_token(K8S), WorkloadCertType::Client, &[])
        };
        let reply = handler
            .handle(WorkloadIssuanceRequest::Issue(stranger), now)
            .await;
        assert!(matches!(reply, WorkloadIssuanceReply::UnknownSession));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_credential_that_does_not_open_is_refused_the_session_is_spent_and_cert_issuer_is_not_asked()
     {
        let server = MockServer::start().await;
        let mut handler = handler(&server).await;
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;
        let other = opened(&mut handler, now).await;

        // Sealed to another session's key.
        let mut wrong_key = sealed(&other, &make_token(K8S), WorkloadCertType::Client, &[]);
        wrong_key.session_id = offer.session_id.clone();
        let reply = handler
            .handle(WorkloadIssuanceRequest::Issue(wrong_key), now)
            .await;
        assert!(
            matches!(
                reply,
                WorkloadIssuanceReply::Issuance(IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::TokenInvalid
                })
            ),
            "{reply:?}"
        );

        // Changed on the way.
        let mut tampered = sealed(&other, &make_token(K8S), WorkloadCertType::Client, &[]);
        let mut json = serde_json::to_value(&tampered.sealed_credential).unwrap();
        let first = json["ciphertext"][0].as_u64().unwrap();
        json["ciphertext"][0] = serde_json::json!(first ^ 1);
        tampered.sealed_credential = serde_json::from_value(json).unwrap();
        let reply = handler
            .handle(WorkloadIssuanceRequest::Issue(tampered), now)
            .await;
        assert!(matches!(
            reply,
            WorkloadIssuanceReply::Issuance(IssuanceReply::Refused {
                reason: IssuanceRefusalReason::TokenInvalid
            })
        ));

        // Both sessions are spent: a second try on either finds nothing.
        for id in [&offer.session_id, &other.session_id] {
            let again = SealedIssuanceRequest {
                session_id: id.clone(),
                ..sealed(&offer, &make_token(K8S), WorkloadCertType::Client, &[])
            };
            let reply = handler
                .handle(WorkloadIssuanceRequest::Issue(again), now)
                .await;
            assert!(matches!(reply, WorkloadIssuanceReply::UnknownSession));
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_full_table_says_busy_and_a_waiting_session_still_works() {
        let server = MockServer::start().await;
        kubernetes_pod(&server, "pod-1").await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response_with_serial("client-1")),
            )
            .mount(&server)
            .await;
        let issuance = handler_for(&server, spawn_test_ledger().await).await;
        let mut handler = WorkloadIssuanceHandler::with_sessions(
            issuance,
            Sessions::new(Duration::from_secs(60), 1),
            OWN_IDENTITY.to_string(),
        )
        .with_signer(a_signer(the_ca(), time::Duration::hours(1)));
        let now = Instant::now();
        let offer = opened(&mut handler, now).await;
        let busy = handler
            .handle(WorkloadIssuanceRequest::OpenSession, now)
            .await;
        assert!(matches!(busy, WorkloadIssuanceReply::Busy));
        let reply = handler
            .handle(
                WorkloadIssuanceRequest::Issue(sealed(
                    &offer,
                    &make_token(K8S),
                    WorkloadCertType::Client,
                    &[],
                )),
                now,
            )
            .await;
        assert!(issued(&reply), "{reply:?}");
    }
}
