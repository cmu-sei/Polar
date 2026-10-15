//! What one attempt to observe a repository amounts to, as the status the observer reports. Pure: no clock, no
//! network, so every rule can be checked without a cluster.

use git_agent_common::status::{MAX_REASON_CHARS, ObservationState, ObservationStatus};
use polar::graph::nodes::git::RepoId;

/// How an attempt ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The credential the repository names has not arrived.
    PendingCredential,
    /// The credential cannot be used for this repository: bound to another host, or the URL is not one a
    /// credential may be used for, or the CryptoActor could not answer.
    CredentialUnusable(String),
    CloneFailed(String),
    FetchFailed(String),
    /// Fetched, and published what was new. `tips` is where each observed ref points now.
    Observed {
        tips: Vec<(String, String)>,
    },
}

/// What the observer remembers between attempts, to say what the last one means in the run it belongs to.
pub struct StatusTracker {
    repo_id: RepoId,
    consecutive_failures: u32,
    last_success: Option<i64>,
    tips: Vec<(String, String)>,
}

impl StatusTracker {
    pub fn new(repo_id: RepoId) -> Self {
        Self {
            repo_id,
            consecutive_failures: 0,
            last_success: None,
            tips: Vec::new(),
        }
    }

    /// The status after an attempt that ended as `outcome` at `now_ms`.
    pub fn record(&mut self, outcome: Outcome, now_ms: i64) -> ObservationStatus {
        let (state, reason) = match outcome {
            Outcome::PendingCredential => {
                // Waiting is not failing: nothing went wrong, and the count is of things that did.
                self.consecutive_failures = 0;
                (ObservationState::PendingCredential, None)
            }
            Outcome::CredentialUnusable(why) => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                (
                    ObservationState::Failing,
                    Some(format!("the credential cannot be used: {why}")),
                )
            }
            Outcome::CloneFailed(why) => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                (
                    ObservationState::Failing,
                    Some(format!("the clone failed: {why}")),
                )
            }
            Outcome::FetchFailed(why) => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                (
                    ObservationState::Failing,
                    Some(format!("the fetch failed: {why}")),
                )
            }
            Outcome::Observed { mut tips } => {
                self.consecutive_failures = 0;
                self.last_success = Some(now_ms);
                tips.sort();
                self.tips = tips;
                (ObservationState::Observing, None)
            }
        };
        ObservationStatus {
            repo_id: self.repo_id.clone(),
            state,
            reason: reason.map(|r| sanitize_reason(&r)),
            last_success: self.last_success,
            tips: self.tips.clone(),
            consecutive_failures: self.consecutive_failures,
            observed_at: now_ms,
        }
    }
}

/// Makes the text of an error fit to be shown to people and written to the graph: one line, no login in any URL,
/// no longer than [`MAX_REASON_CHARS`]. An error from a remote or from libgit2 can be anything; none of it is
/// trusted to be tidy.
pub fn sanitize_reason(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let without_logins = strip_url_logins(&one_line);
    let cleaned: String = without_logins.chars().filter(|c| !c.is_control()).collect();
    if cleaned.chars().count() <= MAX_REASON_CHARS {
        return cleaned;
    }
    let mut cut: String = cleaned.chars().take(MAX_REASON_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// `https://user:secret@host/path` becomes `https://host/path`, wherever it appears in the text.
fn strip_url_logins(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        // The authority ends at the first `/`, `?`, `#` or space; a login is whatever comes before the last `@`
        // inside it.
        let end = tail
            .find(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
            .unwrap_or(tail.len());
        let (authority, after) = tail.split_at(end);
        match authority.rfind('@') {
            Some(i) => out.push_str(&authority[i + 1..]),
            None => out.push_str(authority),
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> StatusTracker {
        StatusTracker::new(RepoId::new("https_github.com_o_r".into()))
    }

    fn tip(name: &str, oid: &str) -> (String, String) {
        (name.to_string(), oid.to_string())
    }

    fn observed(oid: &str) -> Outcome {
        Outcome::Observed {
            tips: vec![tip("refs/remotes/origin/main", oid)],
        }
    }

    #[test]
    fn a_repository_waiting_for_its_credential_is_waiting_not_failing() {
        let s = tracker().record(Outcome::PendingCredential, 10);
        assert_eq!(s.state, ObservationState::PendingCredential);
        assert_eq!(s.reason, None);
        assert_eq!(s.consecutive_failures, 0);
        assert_eq!(s.last_success, None);
        assert_eq!(s.observed_at, 10);
    }

    #[test]
    fn a_fetch_makes_it_observing_with_where_each_ref_points_and_when() {
        let s = tracker().record(
            Outcome::Observed {
                tips: vec![
                    tip("refs/remotes/origin/z", "2"),
                    tip("refs/remotes/origin/a", "1"),
                ],
            },
            50,
        );
        assert_eq!(s.state, ObservationState::Observing);
        assert_eq!(s.last_success, Some(50));
        assert_eq!(
            s.tips,
            vec![
                tip("refs/remotes/origin/a", "1"),
                tip("refs/remotes/origin/z", "2")
            ],
            "sorted, so the same tips are always the same status"
        );
    }

    #[test]
    fn each_kind_of_failure_says_which_it_was() {
        let credential =
            tracker().record(Outcome::CredentialUnusable("bound to gitlab.com".into()), 1);
        let clone = tracker().record(Outcome::CloneFailed("auth required".into()), 1);
        let fetch = tracker().record(Outcome::FetchFailed("timed out".into()), 1);
        for s in [&credential, &clone, &fetch] {
            assert_eq!(s.state, ObservationState::Failing);
        }
        assert_eq!(
            credential.reason.as_deref(),
            Some("the credential cannot be used: bound to gitlab.com")
        );
        assert_eq!(
            clone.reason.as_deref(),
            Some("the clone failed: auth required")
        );
        assert_eq!(fetch.reason.as_deref(), Some("the fetch failed: timed out"));
    }

    #[test]
    fn failures_in_a_row_are_counted_and_a_success_ends_the_run() {
        let mut t = tracker();
        assert_eq!(
            t.record(Outcome::CloneFailed("x".into()), 1)
                .consecutive_failures,
            1
        );
        assert_eq!(
            t.record(Outcome::FetchFailed("x".into()), 2)
                .consecutive_failures,
            2
        );
        assert_eq!(
            t.record(Outcome::CredentialUnusable("x".into()), 3)
                .consecutive_failures,
            3
        );
        let ok = t.record(observed("a"), 4);
        assert_eq!(ok.consecutive_failures, 0);
        assert_eq!(
            ok.reason, None,
            "the old reason does not outlive the failure it explained"
        );
        assert_eq!(
            t.record(Outcome::FetchFailed("x".into()), 5)
                .consecutive_failures,
            1
        );
    }

    #[test]
    fn waiting_ends_a_run_of_failures_because_the_cause_has_changed() {
        let mut t = tracker();
        t.record(Outcome::CloneFailed("x".into()), 1);
        t.record(Outcome::CloneFailed("x".into()), 2);
        let s = t.record(Outcome::PendingCredential, 3);
        assert_eq!(s.consecutive_failures, 0);
        assert_eq!(s.reason, None);
    }

    #[test]
    fn a_failure_after_a_success_still_reports_the_last_success_and_where_the_refs_were() {
        let mut t = tracker();
        t.record(observed("abc"), 100);
        let s = t.record(Outcome::FetchFailed("network".into()), 200);
        assert_eq!(s.state, ObservationState::Failing);
        assert_eq!(s.last_success, Some(100));
        assert_eq!(s.tips, vec![tip("refs/remotes/origin/main", "abc")]);
        assert_eq!(s.observed_at, 200);
    }

    #[test]
    fn a_pod_that_has_never_fetched_has_no_last_success_to_claim() {
        let mut t = tracker();
        let s = t.record(Outcome::CloneFailed("x".into()), 5);
        assert_eq!(s.last_success, None);
        assert!(s.tips.is_empty());
    }

    #[test]
    fn the_count_does_not_wrap() {
        let mut t = tracker();
        t.consecutive_failures = u32::MAX;
        assert_eq!(
            t.record(Outcome::CloneFailed("x".into()), 1)
                .consecutive_failures,
            u32::MAX
        );
    }

    #[test]
    fn a_reason_is_one_line() {
        assert_eq!(sanitize_reason("a\nb\r\n\tc   d"), "a b c d");
    }

    #[test]
    fn a_login_in_a_url_is_removed_wherever_it_appears() {
        assert_eq!(
            sanitize_reason(
                "failed https://user:s3cret@github.com/o/r.git and https://tok@host:8443/x"
            ),
            "failed https://github.com/o/r.git and https://host:8443/x"
        );
        assert_eq!(
            sanitize_reason("https://github.com/o/r.git"),
            "https://github.com/o/r.git"
        );
        assert_eq!(
            sanitize_reason("see https://a.example/p?next=b@c"),
            "see https://a.example/p?next=b@c",
            "an @ in the path or query is not a login"
        );
    }

    #[test]
    fn a_login_with_an_at_sign_in_the_password_is_all_removed() {
        assert_eq!(sanitize_reason("https://u:p@ss@host/x"), "https://host/x");
    }

    #[test]
    fn control_characters_are_dropped() {
        assert_eq!(sanitize_reason("a\u{0}b\u{7}c"), "abc");
    }

    #[test]
    fn a_long_reason_is_cut_to_the_limit_by_characters_and_says_so() {
        let long = "é".repeat(MAX_REASON_CHARS + 50);
        let cut = sanitize_reason(&long);
        assert_eq!(cut.chars().count(), MAX_REASON_CHARS);
        assert!(cut.ends_with('…'));
        let exact = "x".repeat(MAX_REASON_CHARS);
        assert_eq!(sanitize_reason(&exact), exact, "at the limit is left alone");
    }

    #[test]
    fn the_reason_in_a_status_is_cleaned() {
        let s = tracker().record(
            Outcome::CloneFailed("cannot open https://u:secret@github.com/o/r.git\nauth".into()),
            1,
        );
        assert_eq!(
            s.reason.as_deref(),
            Some("the clone failed: cannot open https://github.com/o/r.git auth")
        );
    }
}
