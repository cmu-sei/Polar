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

//! What the custodian fetches for its workload (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian"),
//! as a state machine with no actors and no network: given the plan `polar-init` gave, it says what to publish next,
//! and given a reply, what it means for the custody.
//!
//! In order: the workload's client certificate, its server certificate, its escrow key. Each certificate's key and CSR
//! are made in the custody, and the certificate the credentials agent issues is judged and installed as `polar-init`'s
//! would be. The escrow key is retrieved, sealed to an ephemeral this machine opens alone; if none is escrowed, one is
//! made in the custody, its certificate issued, and the key sealed to the credentials agent's key as the daemon pinned
//! it, and stored; while another pod of a multi-instance workload is being issued it, the custodian waits and asks again,
//! and retrieves the key that pod escrowed. Any other refusal is final: the custody is told, and `polar-init` with it, and the daemon ends so that it
//! starts again empty (`crate::end_once_failed`). A reply that never comes is asked for again.

use credentials_agent_common::agent_escrow_issuance::{
    AgentEscrowIssuanceRefusalReason, AgentEscrowIssuanceReply, AgentEscrowIssuanceRequest,
};
use credentials_agent_common::agent_escrow_keypair_bootstrap::{
    RetrieveAgentEscrowKeypairReply, RetrieveAgentEscrowKeypairRequest,
    agent_escrow_keypair_secret_name,
};
use credentials_agent_common::custodian::CustodianIssuanceRequest;
use credentials_agent_common::ephemeral_exchange::EphemeralSession;
use credentials_agent_common::issuance::{IssuanceReply, WorkloadCertType};
use credentials_agent_common::secret::{StoreRefusalReason, StoreSecretReply, StoreSecretRequest};
use credentials_agent_common::{
    CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC,
    CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC, CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC,
    CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC,
    CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
    CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC,
    CREDENTIALS_STORE_SECRET_REPLIES_TOPIC, CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
};
use polar::request_reply::Envelope;
use polar_custody::custody::{Custody, Plan};
use polar_custody::protocol::Slot;
use std::collections::VecDeque;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// How long a request waits for its reply before it is asked again.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a custodian told its workload's escrow key is being issued (by another pod's custodian) waits before it asks
/// for the key again (docs/architecture/authority-model.md, "Multi-instance workloads").
pub const BEING_ISSUED_WAIT: Duration = Duration::from_secs(2);

/// The topics a custodian's replies arrive on.
pub const REPLY_TOPICS: [&str; 4] = [
    CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC,
    CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
    CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC,
    CREDENTIALS_STORE_SECRET_REPLIES_TOPIC,
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// One of the workload's certificates, with a server certificate's names, and once asked the CSR it was asked
    /// with: asked again with the same, so the same request is admitted again (docs/architecture/deployments.md, "A
    /// retry is not a second request").
    Certificate(Slot, Vec<String>, Option<String>),
    RetrieveEscrow,
    /// The escrow key's certificate, and once asked the CSR it was asked with, as a certificate's.
    IssueEscrow(Option<String>),
    StoreEscrow,
}

/// Something to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub topic: &'static str,
    pub payload: Vec<u8>,
}

/// What the custodian is to do next.
#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    Send(Outgoing),
    /// Nothing until a reply, or the credentials agent's key pinned.
    Wait,
    /// Everything is fetched: the custody says so.
    Done,
    /// It cannot be fetched: the custody says so, `polar-init` stops, and the daemon ends to start again empty.
    Failed(String),
}

struct Waiting {
    correlation_id: String,
    step: Step,
    reply_topic: &'static str,
    session: Option<EphemeralSession>,
    sent_at: Instant,
}

pub struct Bootstrap {
    steps: VecDeque<Step>,
    waiting: Option<Waiting>,
    finished: bool,
    /// Nothing is asked before this: the escrow key is being issued to another pod, which escrows it.
    not_before: Option<Instant>,
}

fn encode<T>(correlation_id: &str, payload: T) -> Result<Vec<u8>, String>
where
    T: rkyv::Archive
        + for<'a> rkyv::Serialize<
            rkyv::api::high::HighSerializer<
                rkyv::util::AlignedVec,
                rkyv::ser::allocator::ArenaHandle<'a>,
                rkyv::rancor::Error,
            >,
        >,
{
    rkyv::to_bytes::<rkyv::rancor::Error>(&Envelope {
        correlation_id: correlation_id.to_string(),
        payload,
    })
    .map(|bytes| bytes.to_vec())
    .map_err(|e| format!("a request does not encode: {e}"))
}

fn decode<T>(payload: &[u8]) -> Option<Envelope<T>>
where
    T: rkyv::Archive,
    T::Archived: for<'a> rkyv::bytecheck::CheckBytes<rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>>
        + rkyv::Deserialize<T, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>,
{
    rkyv::from_bytes::<Envelope<T>, rkyv::rancor::Error>(payload).ok()
}

impl Bootstrap {
    /// What `plan` asks for, in order.
    pub fn new(plan: &Plan) -> Self {
        let mut steps = VecDeque::new();
        if plan.client {
            steps.push_back(Step::Certificate(Slot::Client, Vec::new(), None));
        }
        if let Some(names) = &plan.server {
            steps.push_back(Step::Certificate(Slot::Server, names.clone(), None));
        }
        if plan.escrow {
            steps.push_back(Step::RetrieveEscrow);
        }
        Self {
            steps,
            waiting: None,
            finished: false,
            not_before: None,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    fn fail(&mut self, custody: &RwLock<Custody>, why: String) -> Progress {
        warn!("the custodian cannot fetch what it was asked to: {why}");
        self.finished = true;
        self.steps.clear();
        self.waiting = None;
        write(custody).set_failed(why.clone());
        Progress::Failed(why)
    }

    /// The next thing to publish, if anything: a step started, a request asked again once its reply is overdue, or
    /// the end. `pinned` is the credentials agent's key, once the daemon has pinned it.
    pub fn next(
        &mut self,
        custody: &RwLock<Custody>,
        pinned: Option<&rsa::RsaPublicKey>,
        now: Instant,
    ) -> Progress {
        if self.finished {
            return Progress::Wait;
        }
        if let Some(waiting) = &self.waiting {
            if now.duration_since(waiting.sent_at) < REPLY_TIMEOUT {
                return Progress::Wait;
            }
            warn!(step = ?waiting.step, "no reply in time: asking again");
            let step = waiting.step.clone();
            self.waiting = None;
            self.steps.push_front(step);
        }
        if self.not_before.is_some_and(|at| now < at) {
            return Progress::Wait;
        }
        self.not_before = None;
        let Some(step) = self.steps.pop_front() else {
            self.finished = true;
            write(custody).set_ready();
            info!("the custodian has fetched everything it was asked to");
            return Progress::Done;
        };
        // A key is made once for each request: one asked again is asked with the CSR it was asked with first.
        let step = match step {
            Step::Certificate(slot, names, None) => {
                match write(custody).workload_csr(slot, names.clone()) {
                    Ok(csr) => Step::Certificate(slot, names, Some(csr)),
                    Err(refusal) => return self.fail(custody, format!("{slot:?}: {refusal}")),
                }
            }
            Step::IssueEscrow(None) => match write(custody).escrow_csr() {
                Ok(csr) => Step::IssueEscrow(Some(csr)),
                Err(refusal) => return self.fail(custody, format!("the escrow key: {refusal}")),
            },
            step => step,
        };
        let correlation_id = uuid::Uuid::new_v4().to_string();
        let started = match &step {
            Step::Certificate(_, _, None) | Step::IssueEscrow(None) => {
                return self.fail(custody, format!("{step:?}: asked with no CSR"));
            }
            Step::Certificate(slot, names, Some(csr)) => {
                let cert_type = match slot {
                    Slot::Server => WorkloadCertType::Server,
                    _ => WorkloadCertType::Client,
                };
                encode(
                    &correlation_id,
                    CustodianIssuanceRequest {
                        csr_pem: csr.clone(),
                        cert_type,
                        names: names.clone(),
                    },
                )
                .map(|payload| {
                    (
                        CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC,
                        CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC,
                        payload,
                        None,
                    )
                })
            }
            Step::RetrieveEscrow => {
                let session = EphemeralSession::open();
                encode(
                    &correlation_id,
                    RetrieveAgentEscrowKeypairRequest {
                        ephemeral_public_key: session.public_key(),
                    },
                )
                .map(|payload| {
                    (
                        CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC,
                        CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
                        payload,
                        Some(session),
                    )
                })
            }
            Step::IssueEscrow(Some(csr)) => encode(
                &correlation_id,
                AgentEscrowIssuanceRequest {
                    csr_pem: csr.clone(),
                },
            )
            .map(|payload| {
                (
                    CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC,
                    CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC,
                    payload,
                    None,
                )
            }),
            Step::StoreEscrow => {
                // Sealed to the key the daemon pinned under the CA, and to nothing a caller names.
                let Some(pinned) = pinned else {
                    self.steps.push_front(step);
                    return Progress::Wait;
                };
                let sealed = {
                    let held = read(custody);
                    let identity = held.plan().map(|plan| plan.identity).unwrap_or_default();
                    held.sealed_escrow(pinned).map(|sealed| (sealed, identity))
                };
                let (sealed, identity) = match sealed {
                    Ok(sealed) => sealed,
                    Err(refusal) => {
                        return self.fail(custody, format!("the escrow key: {refusal}"));
                    }
                };
                encode(
                    &correlation_id,
                    StoreSecretRequest {
                        name: agent_escrow_keypair_secret_name(&identity),
                        secret: sealed,
                        labels: Default::default(),
                    },
                )
                .map(|payload| {
                    (
                        CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
                        CREDENTIALS_STORE_SECRET_REPLIES_TOPIC,
                        payload,
                        None,
                    )
                })
            }
        };
        match started {
            Ok((topic, reply_topic, payload, session)) => {
                self.waiting = Some(Waiting {
                    correlation_id,
                    step,
                    reply_topic,
                    session,
                    sent_at: now,
                });
                Progress::Send(Outgoing { topic, payload })
            }
            Err(why) => self.fail(custody, why),
        }
    }

    /// What a reply on `topic` means: nothing if it is not the one awaited, else the custody changed and the next
    /// step (or the end, or a failure).
    pub fn on_reply(
        &mut self,
        topic: &str,
        payload: &[u8],
        custody: &RwLock<Custody>,
        pinned: Option<&rsa::RsaPublicKey>,
        now: Instant,
    ) -> Progress {
        let Some(waiting) = &self.waiting else {
            return Progress::Wait;
        };
        if topic != waiting.reply_topic {
            return Progress::Wait;
        }
        // Each reply topic is every requester's: only the one awaited, by its correlation id, is this custodian's.
        let ours = |id: &str| id == waiting.correlation_id;
        let outcome: Result<Option<Step>, String> = match &waiting.step {
            Step::Certificate(slot, _, _) => match decode::<IssuanceReply>(payload) {
                Some(envelope) if ours(&envelope.correlation_id) => match envelope.payload {
                    IssuanceReply::Issued {
                        certificate_pem, ..
                    } => write(custody)
                        .install_workload(*slot, vec![certificate_pem], unix_now())
                        .map(|()| None)
                        .map_err(|refusal| {
                            format!("{slot:?}: the certificate issued is refused: {refusal}")
                        }),
                    IssuanceReply::Refused { reason } => Err(format!(
                        "{slot:?}: the credentials agent refused: {reason:?}"
                    )),
                },
                _ => return Progress::Wait,
            },
            Step::RetrieveEscrow => match decode::<RetrieveAgentEscrowKeypairReply>(payload) {
                Some(envelope) if ours(&envelope.correlation_id) => match envelope.payload {
                    RetrieveAgentEscrowKeypairReply::Found { payload } => {
                        let session = self
                            .waiting
                            .take()
                            .and_then(|waiting| waiting.session)
                            .expect("a retrieval waits with its session");
                        match session.unseal(&payload) {
                            Ok(pem) => match std::str::from_utf8(&pem) {
                                Ok(pem) => write(custody)
                                    .hold_retrieved_escrow(pem)
                                    .map(|()| None)
                                    .map_err(|refusal| format!("the escrowed key: {refusal}")),
                                Err(_) => Err("the escrowed key is not text".to_string()),
                            },
                            Err(e) => Err(format!("the escrowed key does not open: {e}")),
                        }
                    }
                    // Nothing escrowed yet: one is made here.
                    RetrieveAgentEscrowKeypairReply::NotFound => Ok(Some(Step::IssueEscrow(None))),
                    RetrieveAgentEscrowKeypairReply::Refused { reason } => Err(format!(
                        "the escrow key: the credentials agent refused: {reason:?}"
                    )),
                    RetrieveAgentEscrowKeypairReply::Failed { detail } => {
                        // Not a refusal: asked again.
                        warn!("the escrow key could not be retrieved this time: {detail}");
                        Ok(Some(Step::RetrieveEscrow))
                    }
                },
                _ => return Progress::Wait,
            },
            Step::IssueEscrow(_) => match decode::<AgentEscrowIssuanceReply>(payload) {
                Some(envelope) if ours(&envelope.correlation_id) => match envelope.payload {
                    AgentEscrowIssuanceReply::Issued { .. } => Ok(Some(Step::StoreEscrow)),
                    // Another pod of this workload is being issued the one key the workload has: asked for again shortly,
                    // and retrieved once that pod has escrowed it.
                    AgentEscrowIssuanceReply::Refused {
                        reason: AgentEscrowIssuanceRefusalReason::BeingIssued,
                    } => {
                        info!(
                            "the escrow key is being issued to another pod of this workload: asking for it again shortly"
                        );
                        self.not_before = Some(now + BEING_ISSUED_WAIT);
                        Ok(Some(Step::RetrieveEscrow))
                    }
                    AgentEscrowIssuanceReply::Refused { reason } => Err(format!(
                        "the escrow key's certificate: the credentials agent refused: {reason:?}"
                    )),
                },
                _ => return Progress::Wait,
            },
            Step::StoreEscrow => match decode::<StoreSecretReply>(payload) {
                Some(envelope) if ours(&envelope.correlation_id) => match envelope.payload {
                    StoreSecretReply::Stored => write(custody)
                        .hold_pending_escrow()
                        .map(|()| None)
                        .map_err(|refusal| format!("the escrow key: {refusal}")),
                    // Another pod of this workload escrowed one first: that one is the workload's.
                    StoreSecretReply::Refused {
                        reason: StoreRefusalReason::NameTaken,
                    } => Ok(Some(Step::RetrieveEscrow)),
                    StoreSecretReply::Refused { reason } => Err(format!(
                        "the escrow key could not be escrowed: the credentials agent refused: {reason:?}"
                    )),
                    StoreSecretReply::Failed { detail } => {
                        warn!("the escrow key could not be escrowed this time: {detail}");
                        Ok(Some(Step::StoreEscrow))
                    }
                },
                _ => return Progress::Wait,
            },
        };
        self.waiting = None;
        match outcome {
            Ok(then) => {
                if let Some(step) = then {
                    self.steps.push_front(step);
                }
                self.next(custody, pinned, now)
            }
            Err(why) => self.fail(custody, why),
        }
    }
}

fn read(custody: &RwLock<Custody>) -> std::sync::RwLockReadGuard<'_, Custody> {
    custody
        .read()
        .expect("the custody is never held across a panic")
}

fn write(custody: &RwLock<Custody>) -> std::sync::RwLockWriteGuard<'_, Custody> {
    custody
        .write()
        .expect("the custody is never held across a panic")
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cert_issuer_common::custody::{CustodyPermission, custodian_identity};
    use credentials_agent_common::ephemeral_exchange::EphemeralEncryptedPayload;
    use polar_custody::custody::Roles;
    use polar_custody::protocol::{Reply, Request};
    use proptest::prelude::*;

    const X: &str = "kube-consumer.polar.serviceaccount.cluster.local";
    const INSTALLER: Roles = Roles {
        installer: true,
        workload: false,
    };

    struct Ca {
        issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
        pem: String,
    }

    fn ca() -> &'static Ca {
        static CA: std::sync::OnceLock<Ca> = std::sync::OnceLock::new();
        CA.get_or_init(|| {
            let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            let key = rcgen::KeyPair::generate().unwrap();
            let pem = params.self_signed(&key).unwrap().pem();
            Ca {
                issuer: rcgen::Issuer::new(params, key),
                pem,
            }
        })
    }

    /// What cert-issuer signs for `csr_pem`, with `uris` added: for an hour from a minute ago.
    fn signed(csr_pem: &str, uris: &[String]) -> String {
        let mut csr = rcgen::CertificateSigningRequestParams::from_pem(csr_pem).unwrap();
        for uri in uris {
            csr.params
                .subject_alt_names
                .push(rcgen::SanType::URI(uri.clone().try_into().unwrap()));
        }
        let now = unix_now();
        csr.params.not_before =
            rcgen::date_time_ymd(1970, 1, 1) + Duration::from_secs((now - 60) as u64);
        csr.params.not_after =
            rcgen::date_time_ymd(1970, 1, 1) + Duration::from_secs((now + 3600) as u64);
        csr.signed_by(&ca().issuer).unwrap().pem()
    }

    /// A custody whose custodian holds a certificate naming everything, given `plan`.
    fn custody_planned(client: bool, server: Option<Vec<String>>, escrow: bool) -> RwLock<Custody> {
        let mut custody = Custody::new(&ca().pem).unwrap();
        let Reply::Csr(csr) = custody.handle(
            INSTALLER,
            Request::MakeCsr {
                slot: Slot::Custodian,
                identity: custodian_identity(X),
                names: vec![],
            },
            unix_now(),
        ) else {
            panic!("a CSR")
        };
        let names: Vec<String> = CustodyPermission::ALL
            .iter()
            .map(|p| p.to_san_string())
            .collect();
        assert_eq!(
            custody.handle(
                INSTALLER,
                Request::Install {
                    slot: Slot::Custodian,
                    chain_pem: vec![signed(&csr, &names)],
                },
                unix_now(),
            ),
            Reply::Installed
        );
        assert_eq!(
            custody.handle(
                INSTALLER,
                Request::Bootstrap {
                    identity: X.to_string(),
                    client,
                    server,
                    escrow,
                },
                unix_now(),
            ),
            Reply::Installed
        );
        RwLock::new(custody)
    }

    fn sent(progress: Progress) -> Outgoing {
        match progress {
            Progress::Send(outgoing) => outgoing,
            other => panic!("expected a request, got {other:?}"),
        }
    }

    fn request<T>(outgoing: &Outgoing) -> Envelope<T>
    where
        T: rkyv::Archive,
        T::Archived: for<'a> rkyv::bytecheck::CheckBytes<
                rkyv::api::high::HighValidator<'a, rkyv::rancor::Error>,
            > + rkyv::Deserialize<T, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>,
    {
        decode::<T>(&outgoing.payload).expect("a request of its kind")
    }

    fn reply<T>(correlation_id: &str, payload: T) -> Vec<u8>
    where
        T: rkyv::Archive
            + for<'a> rkyv::Serialize<
                rkyv::api::high::HighSerializer<
                    rkyv::util::AlignedVec,
                    rkyv::ser::allocator::ArenaHandle<'a>,
                    rkyv::rancor::Error,
                >,
            >,
    {
        encode(correlation_id, payload).unwrap()
    }

    fn credentials_agent_key() -> &'static rsa::RsaPrivateKey {
        static KEY: std::sync::OnceLock<rsa::RsaPrivateKey> = std::sync::OnceLock::new();
        KEY.get_or_init(|| rsa::RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap())
    }

    fn issued(certificate_pem: String) -> IssuanceReply {
        IssuanceReply::Issued {
            certificate_pem,
            ca_chain_pem: String::new(),
            escrow_public_key_pem: String::new(),
        }
    }

    #[test]
    fn a_plan_with_nothing_to_fetch_is_done_at_once() {
        let custody = custody_planned(false, None, false);
        let plan = read(&custody).plan().unwrap();
        let mut bootstrap = Bootstrap::new(&plan);
        assert_eq!(
            bootstrap.next(&custody, None, Instant::now()),
            Progress::Done
        );
        assert!(read(&custody).is_ready());
    }

    #[test]
    fn it_fetches_the_client_certificate_then_retrieves_the_escrow_key_and_is_ready() {
        let custody = custody_planned(true, None, true);
        let plan = read(&custody).plan().unwrap();
        let mut bootstrap = Bootstrap::new(&plan);
        let now = Instant::now();
        // The client certificate: a CSR for the workload, made here.
        let asked = sent(bootstrap.next(&custody, None, now));
        assert_eq!(asked.topic, CREDENTIALS_CUSTODIAN_ISSUANCE_REQUESTS_TOPIC);
        let envelope = request::<CustodianIssuanceRequest>(&asked);
        assert_eq!(envelope.payload.cert_type, WorkloadCertType::Client);
        assert_eq!(
            cert_issuer_common::csr_san::csr_dns_sans(&envelope.payload.csr_pem).unwrap(),
            vec![X.to_string()]
        );
        let certificate = signed(&envelope.payload.csr_pem, &[]);
        // Somebody else's reply, and another topic's, change nothing.
        let theirs = reply("someone-else", issued(certificate.clone()));
        assert_eq!(
            bootstrap.on_reply(
                CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC,
                &theirs,
                &custody,
                None,
                now
            ),
            Progress::Wait
        );
        let ours = reply(&envelope.correlation_id, issued(certificate.clone()));
        assert_eq!(
            bootstrap.on_reply(
                CREDENTIALS_STORE_SECRET_REPLIES_TOPIC,
                &ours,
                &custody,
                None,
                now
            ),
            Progress::Wait
        );
        // Ours: installed, and on to the escrow key.
        let asked = sent(bootstrap.on_reply(
            CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC,
            &ours,
            &custody,
            None,
            now,
        ));
        assert_eq!(read(&custody).chain(Slot::Client), Some(vec![certificate]));
        assert_eq!(
            asked.topic,
            CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC
        );
        let envelope = request::<RetrieveAgentEscrowKeypairRequest>(&asked);
        // The credentials agent seals the escrowed key to the ephemeral it was given.
        let escrowed = polar_custody::escrow::EscrowKey::generate().unwrap();
        let pem = {
            let sealed = escrowed
                .sealed_to(&credentials_agent_key().to_public_key())
                .unwrap();
            sealed.decrypt(credentials_agent_key()).unwrap().to_vec()
        };
        let found = RetrieveAgentEscrowKeypairReply::Found {
            payload: EphemeralEncryptedPayload::encrypt(
                &pem,
                &envelope.payload.ephemeral_public_key,
            )
            .unwrap(),
        };
        assert_eq!(
            bootstrap.on_reply(
                CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
                &reply(&envelope.correlation_id, found),
                &custody,
                None,
                now
            ),
            Progress::Done
        );
        assert!(read(&custody).is_ready());
        // The escrow key held is the one escrowed: what is sealed to it unwraps through the daemon.
        let secret =
            polar_escrow::EscrowedSecret::encrypt(b"t", &escrowed.public_key_pem().unwrap())
                .unwrap();
        assert!(matches!(
            read(&custody).handle_read(
                Roles {
                    installer: false,
                    workload: true
                },
                Request::Unwrap {
                    wrapped: secret.wrapped_key().to_vec()
                }
            ),
            Reply::Unwrapped(_)
        ));
    }

    #[test]
    fn with_nothing_escrowed_it_makes_the_key_has_it_issued_and_escrows_it_to_the_pinned_key() {
        let custody = custody_planned(false, None, true);
        let plan = read(&custody).plan().unwrap();
        let mut bootstrap = Bootstrap::new(&plan);
        let now = Instant::now();
        let pinned = credentials_agent_key().to_public_key();
        let asked = sent(bootstrap.next(&custody, None, now));
        let envelope = request::<RetrieveAgentEscrowKeypairRequest>(&asked);
        let asked = sent(bootstrap.on_reply(
            CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
            &reply(
                &envelope.correlation_id,
                RetrieveAgentEscrowKeypairReply::NotFound,
            ),
            &custody,
            None,
            now,
        ));
        assert_eq!(
            asked.topic,
            CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC
        );
        let envelope = request::<AgentEscrowIssuanceRequest>(&asked);
        assert_eq!(
            cert_issuer_common::csr_san::csr_dns_sans(&envelope.payload.csr_pem).unwrap(),
            vec![X.to_string()]
        );
        let issued_escrow = AgentEscrowIssuanceReply::Issued {
            certificate_pem: String::new(),
            ca_chain_pem: String::new(),
            escrow_public_key_pem: String::new(),
        };
        // Until the credentials agent's key is pinned, nothing is sealed.
        assert_eq!(
            bootstrap.on_reply(
                CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC,
                &reply(&envelope.correlation_id, issued_escrow),
                &custody,
                None,
                now,
            ),
            Progress::Wait
        );
        let asked = sent(bootstrap.next(&custody, Some(&pinned), now));
        assert_eq!(asked.topic, CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC);
        let envelope = request::<StoreSecretRequest>(&asked);
        assert_eq!(envelope.payload.name, agent_escrow_keypair_secret_name(X));
        // Sealed to the pinned key: the credentials agent opens it.
        let opened = envelope
            .payload
            .secret
            .decrypt(credentials_agent_key())
            .unwrap();
        let escrowed =
            polar_custody::escrow::EscrowKey::from_pkcs8_pem(std::str::from_utf8(&opened).unwrap())
                .unwrap();
        assert_eq!(
            bootstrap.on_reply(
                CREDENTIALS_STORE_SECRET_REPLIES_TOPIC,
                &reply(&envelope.correlation_id, StoreSecretReply::Stored),
                &custody,
                Some(&pinned),
                now,
            ),
            Progress::Done
        );
        let secret =
            polar_escrow::EscrowedSecret::encrypt(b"t", &escrowed.public_key_pem().unwrap())
                .unwrap();
        assert!(matches!(
            read(&custody).handle_read(
                Roles {
                    installer: false,
                    workload: true
                },
                Request::Unwrap {
                    wrapped: secret.wrapped_key().to_vec()
                }
            ),
            Reply::Unwrapped(_)
        ));
    }

    #[test]
    fn a_refusal_is_final_and_the_custody_says_so() {
        let custody = custody_planned(true, None, false);
        let plan = read(&custody).plan().unwrap();
        let mut bootstrap = Bootstrap::new(&plan);
        let now = Instant::now();
        let asked = sent(bootstrap.next(&custody, None, now));
        let envelope = request::<CustodianIssuanceRequest>(&asked);
        let refused = IssuanceReply::Refused {
            reason:
                credentials_agent_common::issuance::IssuanceRefusalReason::CustodianWindowClosed,
        };
        assert!(matches!(
            bootstrap.on_reply(
                CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC,
                &reply(&envelope.correlation_id, refused),
                &custody,
                None,
                now
            ),
            Progress::Failed(why) if why.contains("CustodianWindowClosed")
        ));
        assert!(matches!(
            read(&custody).handle_read(INSTALLER, Request::Ready),
            Reply::Refused(polar_custody::protocol::Refusal::BootstrapFailed(_))
        ));
        assert_eq!(
            bootstrap.next(&custody, None, now),
            Progress::Wait,
            "and nothing more is asked"
        );
    }

    #[test]
    fn a_reply_that_never_comes_is_asked_for_again_and_the_late_one_is_not_taken() {
        let custody = custody_planned(true, None, false);
        let plan = read(&custody).plan().unwrap();
        let mut bootstrap = Bootstrap::new(&plan);
        let start = Instant::now();
        let first =
            request::<CustodianIssuanceRequest>(&sent(bootstrap.next(&custody, None, start)));
        assert_eq!(
            bootstrap.next(&custody, None, start + Duration::from_secs(1)),
            Progress::Wait
        );
        let again = request::<CustodianIssuanceRequest>(&sent(bootstrap.next(
            &custody,
            None,
            start + REPLY_TIMEOUT + Duration::from_secs(1),
        )));
        assert_ne!(again.correlation_id, first.correlation_id);
        // docs/architecture/deployments.md, "A retry is not a second request": asked again for the same key, so the
        // credentials agent's renewal rule admits it again.
        assert_eq!(again.payload.csr_pem, first.payload.csr_pem);
        // The first one's answer, arriving late, is not taken.
        let late = reply(
            &first.correlation_id,
            issued(signed(&first.payload.csr_pem, &[])),
        );
        assert_eq!(
            bootstrap.on_reply(
                CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC,
                &late,
                &custody,
                None,
                start
            ),
            Progress::Wait
        );
        let answer = reply(
            &again.correlation_id,
            issued(signed(&again.payload.csr_pem, &[])),
        );
        assert_eq!(
            bootstrap.on_reply(
                CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC,
                &answer,
                &custody,
                None,
                start
            ),
            Progress::Done
        );
    }

    // docs/architecture/deployments.md, "A retry is not a second request": the escrow key's certificate, its reply
    // overdue, is asked for again for the same key, and the key it is issued for is the one stored.
    #[test]
    fn an_escrow_issuance_whose_reply_never_comes_is_asked_again_for_the_same_key() {
        let custody = custody_planned(false, None, true);
        let plan = read(&custody).plan().unwrap();
        let mut bootstrap = Bootstrap::new(&plan);
        let now = Instant::now();
        let pinned = credentials_agent_key().to_public_key();
        let retrieve = request::<RetrieveAgentEscrowKeypairRequest>(&sent(bootstrap.next(
            &custody,
            Some(&pinned),
            now,
        )));
        let first = request::<AgentEscrowIssuanceRequest>(&sent(bootstrap.on_reply(
            CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
            &reply(
                &retrieve.correlation_id,
                RetrieveAgentEscrowKeypairReply::NotFound,
            ),
            &custody,
            Some(&pinned),
            now,
        )));
        let later = now + REPLY_TIMEOUT + Duration::from_secs(1);
        let again = request::<AgentEscrowIssuanceRequest>(&sent(bootstrap.next(
            &custody,
            Some(&pinned),
            later,
        )));
        assert_ne!(again.correlation_id, first.correlation_id);
        assert_eq!(again.payload.csr_pem, first.payload.csr_pem);
        let store = request::<StoreSecretRequest>(&sent(bootstrap.on_reply(
            CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC,
            &reply(
                &again.correlation_id,
                AgentEscrowIssuanceReply::Issued {
                    certificate_pem: String::new(),
                    ca_chain_pem: String::new(),
                    escrow_public_key_pem: String::new(),
                },
            ),
            &custody,
            Some(&pinned),
            later,
        )));
        // What is stored is the key both requests asked for.
        let stored = store
            .payload
            .secret
            .decrypt(credentials_agent_key())
            .unwrap();
        let stored =
            polar_custody::escrow::EscrowKey::from_pkcs8_pem(std::str::from_utf8(&stored).unwrap())
                .unwrap();
        assert_eq!(
            cert_issuer_common::public_key::csr_key(&stored.csr(&plan.identity).unwrap()),
            cert_issuer_common::public_key::csr_key(&first.payload.csr_pem)
        );
    }

    #[test]
    fn another_pod_escrowing_first_sends_it_back_to_retrieve_that_key() {
        let custody = custody_planned(false, None, true);
        let plan = read(&custody).plan().unwrap();
        let mut bootstrap = Bootstrap::new(&plan);
        let now = Instant::now();
        let pinned = credentials_agent_key().to_public_key();
        let retrieve = request::<RetrieveAgentEscrowKeypairRequest>(&sent(bootstrap.next(
            &custody,
            Some(&pinned),
            now,
        )));
        let issue = request::<AgentEscrowIssuanceRequest>(&sent(bootstrap.on_reply(
            CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
            &reply(
                &retrieve.correlation_id,
                RetrieveAgentEscrowKeypairReply::NotFound,
            ),
            &custody,
            Some(&pinned),
            now,
        )));
        let store = request::<StoreSecretRequest>(&sent(bootstrap.on_reply(
            CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC,
            &reply(
                &issue.correlation_id,
                AgentEscrowIssuanceReply::Issued {
                    certificate_pem: String::new(),
                    ca_chain_pem: String::new(),
                    escrow_public_key_pem: String::new(),
                },
            ),
            &custody,
            Some(&pinned),
            now,
        )));
        let asked = sent(bootstrap.on_reply(
            CREDENTIALS_STORE_SECRET_REPLIES_TOPIC,
            &reply(
                &store.correlation_id,
                StoreSecretReply::Refused {
                    reason: StoreRefusalReason::NameTaken,
                },
            ),
            &custody,
            Some(&pinned),
            now,
        ));
        assert_eq!(
            asked.topic,
            CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC
        );
    }

    /// docs/architecture/authority-model.md, "Multi-instance workloads": told the workload's one escrow key is being
    /// issued to another pod, the custodian does not fail; it waits, asks for the key again, and holds the one that pod
    /// escrowed.
    #[test]
    fn told_the_key_is_being_issued_to_another_pod_it_waits_and_retrieves_that_key() {
        let custody = custody_planned(false, None, true);
        let plan = read(&custody).plan().unwrap();
        let mut bootstrap = Bootstrap::new(&plan);
        let now = Instant::now();
        let pinned = credentials_agent_key().to_public_key();
        let retrieve = request::<RetrieveAgentEscrowKeypairRequest>(&sent(bootstrap.next(
            &custody,
            Some(&pinned),
            now,
        )));
        let issue = request::<AgentEscrowIssuanceRequest>(&sent(bootstrap.on_reply(
            CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
            &reply(
                &retrieve.correlation_id,
                RetrieveAgentEscrowKeypairReply::NotFound,
            ),
            &custody,
            Some(&pinned),
            now,
        )));
        // Told so: nothing asked at once, nothing failed.
        assert_eq!(
            bootstrap.on_reply(
                CREDENTIALS_AGENT_ESCROW_ISSUANCE_REPLIES_TOPIC,
                &reply(
                    &issue.correlation_id,
                    AgentEscrowIssuanceReply::Refused {
                        reason: AgentEscrowIssuanceRefusalReason::BeingIssued,
                    },
                ),
                &custody,
                Some(&pinned),
                now,
            ),
            Progress::Wait
        );
        assert!(!bootstrap.is_finished());
        assert_eq!(
            bootstrap.next(&custody, Some(&pinned), now + BEING_ISSUED_WAIT / 2),
            Progress::Wait
        );
        // Then it asks for the key again, and holds the one the other pod escrowed.
        let later = now + BEING_ISSUED_WAIT;
        let again = request::<RetrieveAgentEscrowKeypairRequest>(&sent(bootstrap.next(
            &custody,
            Some(&pinned),
            later,
        )));
        let escrowed = polar_custody::escrow::EscrowKey::generate().unwrap();
        let pem = escrowed
            .sealed_to(&pinned)
            .unwrap()
            .decrypt(credentials_agent_key())
            .unwrap()
            .to_vec();
        let found = RetrieveAgentEscrowKeypairReply::Found {
            payload: EphemeralEncryptedPayload::encrypt(&pem, &again.payload.ephemeral_public_key)
                .unwrap(),
        };
        assert_eq!(
            bootstrap.on_reply(
                CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REPLIES_TOPIC,
                &reply(&again.correlation_id, found),
                &custody,
                Some(&pinned),
                later,
            ),
            Progress::Done
        );
    }

    proptest! {
        /// Whatever arrives on whatever topic, nothing that is not the reply awaited moves the custodian: it neither
        /// advances nor fails, and the custody is unchanged.
        #[test]
        fn nothing_but_the_reply_awaited_moves_it(
            arrivals in prop::collection::vec((
                prop::sample::select(REPLY_TOPICS.to_vec()),
                prop::collection::vec(any::<u8>(), 0..300),
            ), 0..12),
        ) {
            let custody = custody_planned(true, None, false);
            let plan = read(&custody).plan().unwrap();
            let mut bootstrap = Bootstrap::new(&plan);
            let now = Instant::now();
            let asked = request::<CustodianIssuanceRequest>(&sent(bootstrap.next(&custody, None, now)));
            for (topic, payload) in arrivals {
                prop_assert_eq!(bootstrap.on_reply(topic, &payload, &custody, None, now), Progress::Wait);
            }
            // A well-formed reply under another correlation id is nobody's but its requester's.
            let theirs = reply("not-ours", issued(signed(&asked.payload.csr_pem, &[])));
            prop_assert_eq!(
                bootstrap.on_reply(CREDENTIALS_CUSTODIAN_ISSUANCE_REPLIES_TOPIC, &theirs, &custody, None, now),
                Progress::Wait
            );
            prop_assert!(read(&custody).chain(Slot::Client).is_none());
            prop_assert!(!read(&custody).is_ready());
        }
    }
}
