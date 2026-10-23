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

//! cert-issuer's record of every certificate it signs (docs/architecture/cert-issuer-isolation.md, "Step 2"). The CA
//! is wrapped ([`RecordingCa`]) so that nothing signs around it: a certificate is appended to the log, and the log
//! synced, before it is returned, and one that cannot be recorded is not returned at all. The credentials agent asks
//! for the log, a page at a time ([`RecordingCa::page`]), so that no certificate cert-issuer signs stays unknown to it.

use crate::ca::{CaClient, CaError, CaIssueRequest, IssuedCert};
use cert_issuer_common::record::{RecordEntry, RecordReply, RecordRequest};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// The log on disk, one JSON line per certificate, and what it holds, read once when opened.
pub struct Record {
    path: PathBuf,
    log: String,
    entries: Vec<RecordEntry>,
}

impl Record {
    /// The log at `path`: read back if it is there, made (with an id of its own) if it is not. A last line cut short
    /// (a crash while it was written: its certificate was never returned) is dropped; any other line that does not
    /// read is an error, never a gap.
    pub fn open(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("the record {}: {e}", path.display())),
        };
        let complete = match text.rfind('\n') {
            Some(end) => &text[..=end],
            None => "",
        };
        if complete.len() < text.len() {
            // Cut short: keep what was whole, so the next entry starts on a line of its own.
            std::fs::write(path, complete)
                .map_err(|e| format!("the record {}: {e}", path.display()))?;
        }
        let mut entries = Vec::new();
        for (n, line) in complete.lines().enumerate() {
            let entry: RecordEntry = serde_json::from_str(line)
                .map_err(|e| format!("the record {}, line {}: {e}", path.display(), n + 1))?;
            entries.push(entry);
        }
        let log = match entries.last() {
            Some(last) => last.log.clone(),
            None => uuid::Uuid::new_v4().to_string(),
        };
        Ok(Self {
            path: path.to_path_buf(),
            log,
            entries,
        })
    }

    /// The log's id.
    pub fn log(&self) -> &str {
        &self.log
    }

    /// What the log holds, in order.
    pub fn entries(&self) -> &[RecordEntry] {
        &self.entries
    }

    /// The answer to `request`, at most `limit` entries: those after its number in this log, or from the first entry
    /// when it names another log, or none.
    pub fn page(&self, request: &RecordRequest, limit: usize) -> RecordReply {
        let after = if request.log.as_deref() == Some(self.log.as_str()) {
            request.after
        } else {
            0
        };
        let rest: Vec<&RecordEntry> = self.entries.iter().filter(|e| e.seq > after).collect();
        RecordReply {
            log: self.log.clone(),
            entries: rest.iter().take(limit).map(|e| (*e).clone()).collect(),
            more: rest.len() > limit,
        }
    }

    /// Appends the entry for `issued`, signed as `request` asked, and syncs the log: the next number, this log's id.
    fn append(&mut self, request: &CaIssueRequest, issued: &IssuedCert) -> Result<(), String> {
        let entry = RecordEntry {
            log: self.log.clone(),
            seq: self.entries.last().map_or(1, |e| e.seq + 1),
            serial: issued.serial_number.clone(),
            identity: request.sans.first().cloned().unwrap_or_default(),
            cert_type: request.cert_type.clone(),
            path: request.path,
            instance_binding: request.instance_binding.clone(),
            signed_at: time::OffsetDateTime::now_utc().unix_timestamp(),
            not_after: issued.not_after.unix_timestamp(),
            certificate_pem: issued.certificate_pem.clone(),
        };
        let line = serde_json::to_string(&entry).map_err(|e| e.to_string())?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        file.write_all(format!("{line}\n").as_bytes())
            .and_then(|_| file.sync_data())
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        self.entries.push(entry);
        Ok(())
    }
}

/// A CA that records every certificate it signs before it returns it.
pub struct RecordingCa {
    inner: Arc<dyn CaClient>,
    record: Mutex<Record>,
}

impl RecordingCa {
    pub fn new(inner: Arc<dyn CaClient>, record: Record) -> Self {
        Self {
            inner,
            record: Mutex::new(record),
        }
    }

    /// The answer to `request`, at most `limit` entries.
    pub async fn page(&self, request: &RecordRequest, limit: usize) -> RecordReply {
        self.record.lock().await.page(request, limit)
    }

    /// The reply to a request that arrived on `RECORD_REQUESTS_TOPIC`, published (the broker says) by `published_by`,
    /// or why it is not answered: only one of `internal_callers`, the credentials agent, is answered, and only a
    /// request that reads.
    pub async fn answer(
        &self,
        payload: &[u8],
        published_by: Option<&str>,
        internal_callers: &[String],
    ) -> Result<Vec<u8>, String> {
        let Some(asker) = published_by else {
            return Err("a request for the record with no verified publisher".to_string());
        };
        if !internal_callers.iter().any(|c| c == asker) {
            return Err(format!(
                "{asker} asked for the record, and only the credentials agent may"
            ));
        }
        let request: RecordRequest = serde_json::from_slice(payload)
            .map_err(|e| format!("a request for the record that does not read: {e}"))?;
        let reply = self
            .page(&request, cert_issuer_common::record::RECORD_PAGE)
            .await;
        serde_json::to_vec(&reply).map_err(|e| e.to_string())
    }
}

#[async_trait::async_trait]
impl CaClient for RecordingCa {
    async fn issue(&self, req: CaIssueRequest) -> Result<IssuedCert, CaError> {
        let issued = self.inner.issue(req.clone()).await?;
        self.record
            .lock()
            .await
            .append(&req, &issued)
            .map_err(|e| {
                CaError::SigningFailed(format!(
                    "the certificate could not be recorded, so it is not returned: {e}"
                ))
            })?;
        Ok(issued)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cert_issuer_common::CertType;
    use cert_issuer_common::record::IssuePath;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// A CA that signs nothing real: each certificate is its serial, counting up.
    #[derive(Default)]
    struct CountingCa(AtomicU64);

    #[async_trait::async_trait]
    impl CaClient for CountingCa {
        async fn issue(&self, req: CaIssueRequest) -> Result<IssuedCert, CaError> {
            let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(IssuedCert {
                certificate_pem: format!("pem of {} #{n}", req.sans[0]),
                ca_chain_pem: String::new(),
                serial_number: format!("{n:02x}"),
                not_after: time::OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap(),
            })
        }
    }

    fn request(identity: &str, cert_type: CertType, path: IssuePath) -> CaIssueRequest {
        CaIssueRequest {
            csr_pem: String::new(),
            sans: vec![identity.to_string()],
            lifetime: Duration::from_secs(3600),
            cert_type,
            subject_dn: Default::default(),
            path,
            instance_binding: Some("pod-uid".to_string()),
        }
    }

    fn recording(path: &Path) -> RecordingCa {
        RecordingCa::new(Arc::new(CountingCa::default()), Record::open(path).unwrap())
    }

    #[tokio::test]
    async fn every_signing_is_recorded_with_how_it_came_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("issued.jsonl");
        let ca = recording(&log);
        for (identity, path) in [
            ("cassini.polar.svc.cluster.local", IssuePath::TokenOnly),
            (
                "kube-observer.polar.serviceaccount.cluster.local",
                IssuePath::Vouched,
            ),
            ("an-account", IssuePath::Internal),
            ("cert-issuer.polar.svc.cluster.local", IssuePath::Own),
        ] {
            let issued = ca
                .issue(request(identity, CertType::Client, path))
                .await
                .unwrap();
            assert!(issued.certificate_pem.contains(identity));
        }
        let record = Record::open(&log).unwrap();
        let entries = record.entries();
        assert_eq!(
            entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            entries.iter().map(|e| e.path).collect::<Vec<_>>(),
            vec![
                IssuePath::TokenOnly,
                IssuePath::Vouched,
                IssuePath::Internal,
                IssuePath::Own
            ]
        );
        assert_eq!(
            entries[1].identity,
            "kube-observer.polar.serviceaccount.cluster.local"
        );
        assert_eq!(entries[1].serial, "02");
        assert_eq!(entries[1].instance_binding.as_deref(), Some("pod-uid"));
        assert_eq!(entries[1].not_after, 2_000_000_000);
        assert!(entries.iter().all(|e| e.log == record.log()));
    }

    #[tokio::test]
    async fn the_numbers_go_on_rising_in_the_same_log_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("issued.jsonl");
        let first = recording(&log);
        first
            .issue(request("a", CertType::Client, IssuePath::TokenOnly))
            .await
            .unwrap();
        let id = Record::open(&log).unwrap().log().to_string();
        drop(first);
        let again = recording(&log);
        again
            .issue(request("b", CertType::Server, IssuePath::Vouched))
            .await
            .unwrap();
        let record = Record::open(&log).unwrap();
        assert_eq!(record.log(), id, "the same log");
        assert_eq!(
            record.entries().iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn a_new_log_has_an_id_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let a = Record::open(&dir.path().join("a.jsonl")).unwrap();
        let b = Record::open(&dir.path().join("b.jsonl")).unwrap();
        assert!(!a.log().is_empty());
        assert_ne!(a.log(), b.log());
    }

    #[tokio::test]
    async fn a_certificate_that_cannot_be_recorded_is_not_returned() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("issued.jsonl");
        let ca = recording(&log);
        ca.issue(request("a", CertType::Client, IssuePath::TokenOnly))
            .await
            .unwrap();
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o444)).unwrap();
        let refused = ca
            .issue(request("b", CertType::Client, IssuePath::TokenOnly))
            .await;
        assert!(refused.is_err(), "no certificate leaves unrecorded");
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(Record::open(&log).unwrap().entries().len(), 1);
    }

    #[tokio::test]
    async fn a_last_line_cut_short_by_a_crash_is_dropped_and_the_log_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("issued.jsonl");
        let ca = recording(&log);
        ca.issue(request("a", CertType::Client, IssuePath::TokenOnly))
            .await
            .unwrap();
        drop(ca);
        // A write the crash cut short: its certificate was never returned.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&log)
            .unwrap()
            .write_all(b"{\"log\":\"x\",\"se")
            .unwrap();
        let again = recording(&log);
        again
            .issue(request("b", CertType::Client, IssuePath::TokenOnly))
            .await
            .unwrap();
        let record = Record::open(&log).unwrap();
        assert_eq!(
            record
                .entries()
                .iter()
                .map(|e| (e.identity.as_str(), e.seq))
                .collect::<Vec<_>>(),
            vec![("a", 1), ("b", 2)]
        );
    }

    #[test]
    fn a_log_that_cannot_be_read_is_an_error_never_a_new_log() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("issued.jsonl");
        std::fs::write(&log, "").unwrap();
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o000)).unwrap();
        let opened = Record::open(&log);
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            opened.is_err(),
            "a history it cannot read is not one it may start again"
        );
    }

    #[test]
    fn a_malformed_line_before_the_last_is_an_error_not_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("issued.jsonl");
        std::fs::write(&log, "not an entry\n{}\n").unwrap();
        assert!(Record::open(&log).is_err());
    }

    #[tokio::test]
    async fn the_record_is_answered_a_page_at_a_time_and_from_the_start_for_another_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("issued.jsonl");
        let ca = recording(&log);
        for i in 0..5 {
            ca.issue(request(
                &format!("w{i}"),
                CertType::Client,
                IssuePath::TokenOnly,
            ))
            .await
            .unwrap();
        }
        let id = Record::open(&log).unwrap().log().to_string();

        let first = ca
            .page(
                &RecordRequest {
                    log: None,
                    after: 0,
                },
                2,
            )
            .await;
        assert_eq!(first.log, id);
        assert_eq!(
            first.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(first.more);
        let next = ca
            .page(
                &RecordRequest {
                    log: Some(id.clone()),
                    after: 2,
                },
                2,
            )
            .await;
        assert_eq!(
            next.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert!(next.more);
        let last = ca
            .page(
                &RecordRequest {
                    log: Some(id.clone()),
                    after: 4,
                },
                2,
            )
            .await;
        assert_eq!(
            last.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![5]
        );
        assert!(!last.more);
        // Exactly a page left: no more after it.
        let exact = ca
            .page(
                &RecordRequest {
                    log: Some(id.clone()),
                    after: 3,
                },
                2,
            )
            .await;
        assert_eq!(
            exact.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert!(!exact.more);
        let done = ca
            .page(
                &RecordRequest {
                    log: Some(id.clone()),
                    after: 5,
                },
                2,
            )
            .await;
        assert!(done.entries.is_empty() && !done.more);

        // A log it no longer has: everything, from the first entry.
        let other = ca
            .page(
                &RecordRequest {
                    log: Some("another".to_string()),
                    after: 4,
                },
                10,
            )
            .await;
        assert_eq!(other.entries.len(), 5);
        assert_eq!(other.log, id);
    }

    const CREDENTIALS_AGENT: &str = "credentials-agent.polar.serviceaccount.cluster.local";

    #[tokio::test]
    async fn the_credentials_agent_s_request_is_answered_with_a_page_of_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let ca = recording(&dir.path().join("issued.jsonl"));
        for i in 0..3 {
            ca.issue(request(
                &format!("w{i}"),
                CertType::Client,
                IssuePath::TokenOnly,
            ))
            .await
            .unwrap();
        }
        let asked = serde_json::to_vec(&RecordRequest {
            log: None,
            after: 1,
        })
        .unwrap();
        let reply = ca
            .answer(
                &asked,
                Some(CREDENTIALS_AGENT),
                &[CREDENTIALS_AGENT.to_string()],
            )
            .await
            .unwrap();
        let reply: RecordReply = serde_json::from_slice(&reply).unwrap();
        assert_eq!(
            reply
                .entries
                .iter()
                .map(|e| e.identity.as_str())
                .collect::<Vec<_>>(),
            vec!["w0", "w1", "w2"]
        );
        assert!(!reply.more);
    }

    #[tokio::test]
    async fn a_request_from_anyone_else_or_no_one_is_not_answered() {
        let dir = tempfile::tempdir().unwrap();
        let ca = recording(&dir.path().join("issued.jsonl"));
        ca.issue(request("w", CertType::Client, IssuePath::TokenOnly))
            .await
            .unwrap();
        let asked = serde_json::to_vec(&RecordRequest {
            log: None,
            after: 0,
        })
        .unwrap();
        let callers = [CREDENTIALS_AGENT.to_string()];
        for who in [
            Some("kube-observer.polar.serviceaccount.cluster.local"),
            None,
        ] {
            assert!(ca.answer(&asked, who, &callers).await.is_err(), "{who:?}");
        }
        assert!(
            ca.answer(&asked, Some(CREDENTIALS_AGENT), &[])
                .await
                .is_err(),
            "no one is an internal caller"
        );
    }

    #[tokio::test]
    async fn a_request_that_does_not_read_is_not_answered() {
        let dir = tempfile::tempdir().unwrap();
        let ca = recording(&dir.path().join("issued.jsonl"));
        let refused = ca
            .answer(
                b"not a request",
                Some(CREDENTIALS_AGENT),
                &[CREDENTIALS_AGENT.to_string()],
            )
            .await;
        assert!(refused.is_err());
    }

    #[tokio::test]
    async fn an_answer_is_at_most_a_page() {
        let dir = tempfile::tempdir().unwrap();
        let ca = recording(&dir.path().join("issued.jsonl"));
        for i in 0..(cert_issuer_common::record::RECORD_PAGE + 1) {
            ca.issue(request(
                &format!("w{i}"),
                CertType::Client,
                IssuePath::TokenOnly,
            ))
            .await
            .unwrap();
        }
        let asked = serde_json::to_vec(&RecordRequest {
            log: None,
            after: 0,
        })
        .unwrap();
        let reply: RecordReply = serde_json::from_slice(
            &ca.answer(
                &asked,
                Some(CREDENTIALS_AGENT),
                &[CREDENTIALS_AGENT.to_string()],
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(reply.entries.len(), cert_issuer_common::record::RECORD_PAGE);
        assert!(reply.more);
    }

    fn path() -> impl Strategy<Value = IssuePath> {
        prop_oneof![
            Just(IssuePath::TokenOnly),
            Just(IssuePath::Vouched),
            Just(IssuePath::Internal),
            Just(IssuePath::Own)
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// The log read back is the signings made, in order, numbered from 1, whatever they were; and paging through
        /// it, any page size, gives every entry once, in order.
        #[test]
        fn the_log_is_the_signings_made_in_order(
            signings in proptest::collection::vec(("[a-z]{1,8}", path(), any::<bool>()), 0..12),
            limit in 1usize..5,
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("issued.jsonl");
            let ca = recording(&log);
            runtime.block_on(async {
                for (identity, path, server) in &signings {
                    let cert_type = if *server { CertType::Server } else { CertType::Client };
                    ca.issue(request(identity, cert_type, *path)).await.unwrap();
                }
            });
            let record = Record::open(&log).unwrap();
            let read: Vec<(String, IssuePath, u64)> =
                record.entries().iter().map(|e| (e.identity.clone(), e.path, e.seq)).collect();
            let made: Vec<(String, IssuePath, u64)> = signings
                .iter()
                .enumerate()
                .map(|(i, (identity, path, _))| (identity.clone(), *path, i as u64 + 1))
                .collect();
            prop_assert_eq!(read, made);

            let mut seen = Vec::new();
            let mut after = 0;
            // Bounded: a page that never says it is the last fails here rather than running for ever.
            for _ in 0..=signings.len() + 1 {
                let page = record.page(&RecordRequest { log: Some(record.log().to_string()), after }, limit);
                seen.extend(page.entries.iter().map(|e| e.seq));
                after = page.entries.last().map(|e| e.seq).unwrap_or(after);
                if !page.more { break; }
            }
            prop_assert_eq!(seen, (1..=signings.len() as u64).collect::<Vec<_>>());
            let last = record.page(&RecordRequest { log: Some(record.log().to_string()), after }, limit);
            prop_assert!(last.entries.is_empty() && !last.more, "after the last page there is nothing more");
        }
    }
}
