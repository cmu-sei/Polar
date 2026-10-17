//! Analysis scheduler.
//!
//! Turns the graph's record of past analysis runs into a work queue for the
//! build orchestrator. It holds no durable state of its own: on every tick it
//! asks the graph which packages the configured analyzer image has not yet
//! analyzed, and publishes a `BuildRequest` for each to the orchestrator.
//!
//! A package is due unless a run with this analyzer image either succeeded,
//! is still in flight, or has failed `max_failed_attempts` times (see
//! [`PENDING_PACKAGES_QUERY`]). The only in-memory state, [`RecentRequests`],
//! covers the short window between publishing a request and the
//! orchestrator's `PackageAnalysisStarted` reaching the graph.

pub mod supervisor;

use orchestrator_core::types::{BuildRequest, BuildRequestError, SourceRef};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub const SERVICE_NAME: &str = "polar.analysis.scheduler";

/// Packages due for analysis by `$analyzer_image`, oldest purl first.
///
/// - Only cargo packages with a published-archive digest and no purl
///   qualifiers: path and git dependencies carry qualifiers (and no digest),
///   and the `source` property cannot be trusted to tell them apart — it
///   reports "registry" for path crates.
/// - `completed_at` is set on success, failure and cancellation alike, so
///   success is read from the job's `succeeded` state instance.
/// - A run that never reports a terminal event counts as in flight forever.
///   That needs the orchestrator's Unreconciled handling, not a guess here.
pub const PENDING_PACKAGES_QUERY: &str = "\
MATCH (p:Package)
WHERE p.purl STARTS WITH 'pkg:cargo/'
  AND NOT p.purl CONTAINS '?'
  AND p.archive_sha256 IS NOT NULL
  AND NOT EXISTS {
    MATCH (p)<-[a:ANALYZED]-(j:BuildJob)
    WHERE a.analyzer_image = $analyzer_image
      AND (j.completed_at IS NULL
           OR EXISTS { (j)-[:TRANSITIONED_TO]->(:BuildJobState { phase: 'succeeded' }) })
  }
  AND COUNT {
    MATCH (p)<-[a:ANALYZED]-(f:BuildJob)
    WHERE a.analyzer_image = $analyzer_image AND f.failure_reason IS NOT NULL
  } < $max_failed_attempts
RETURN p.purl AS purl, p.archive_sha256 AS sha256
ORDER BY p.purl
LIMIT $limit";

/// Runs of `$analyzer_image` that have started but not finished. The
/// orchestrator has no concurrency limit of its own; the scheduler keeps this
/// at or below `max_in_flight`.
pub const IN_FLIGHT_QUERY: &str = "\
MATCH (j:BuildJob)-[a:ANALYZED]->(:Package)
WHERE a.analyzer_image = $analyzer_image AND j.completed_at IS NULL
RETURN count(DISTINCT j) AS n";

/// How many new requests a tick may publish: the batch size, limited by the
/// free in-flight slots.
pub fn requests_allowed(batch_size: i64, max_in_flight: i64, in_flight: i64) -> i64 {
    batch_size.min(max_in_flight - in_flight).max(0)
}

// ── Configuration ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct SchedulerConfig {
    /// Analyzer image, pinned by digest — its identity in the graph.
    pub analyzer_image: String,
    /// Time between graph queries.
    pub interval: Duration,
    /// Maximum requests published per tick.
    pub batch_size: i64,
    /// Maximum unfinished runs of this analyzer at once, counted from the graph.
    pub max_in_flight: i64,
    /// Stop requesting a package after this many failed runs.
    pub max_failed_attempts: i64,
    /// How long a published request suppresses re-requesting the same package.
    pub request_ttl: Duration,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ConfigError {
    #[error("ANALYZER_IMAGE is required")]
    MissingAnalyzerImage,
    #[error("ANALYZER_IMAGE must be pinned by digest (…@sha256:…), got {0}")]
    UnpinnedAnalyzerImage(String),
    #[error("{0} must be a positive integer, got {1:?}")]
    InvalidNumber(&'static str, String),
}

impl SchedulerConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let analyzer_image = get("ANALYZER_IMAGE").ok_or(ConfigError::MissingAnalyzerImage)?;
        if !analyzer_image.contains("@sha256:") {
            return Err(ConfigError::UnpinnedAnalyzerImage(analyzer_image));
        }
        let number = |key: &'static str, default: u64| -> Result<u64, ConfigError> {
            match get(key) {
                None => Ok(default),
                Some(v) => v
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or(ConfigError::InvalidNumber(key, v)),
            }
        };
        Ok(Self {
            analyzer_image,
            interval: Duration::from_secs(number("ANALYSIS_INTERVAL_SECS", 60)?),
            batch_size: number("ANALYSIS_BATCH_SIZE", 10)? as i64,
            max_in_flight: number("ANALYSIS_MAX_IN_FLIGHT", 20)? as i64,
            max_failed_attempts: number("ANALYSIS_MAX_FAILED_ATTEMPTS", 3)? as i64,
            request_ttl: Duration::from_secs(number("ANALYSIS_REQUEST_TTL_SECS", 900)?),
        })
    }
}

// ── Requests ─────────────────────────────────────────────────────────────────

/// The crates.io download URL for an unqualified `pkg:cargo/<name>@<version>`.
///
/// None for anything else: qualified purls (`?repository_url=…`,
/// `?download_url=…`, `?vcs_url=…`) are not crates.io packages.
pub fn crates_io_download_url(purl: &str) -> Option<String> {
    let rest = purl.strip_prefix("pkg:cargo/")?;
    if rest.contains(['?', '#']) {
        return None;
    }
    let (name, version) = rest.split_once('@')?;
    let name_ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let version_ok = !version.is_empty()
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    (name_ok && version_ok)
        .then(|| format!("https://static.crates.io/crates/{name}/{name}-{version}.crate"))
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RequestError {
    #[error("not a crates.io package: {0}")]
    NotCratesIo(String),
    #[error(transparent)]
    Invalid(#[from] BuildRequestError),
}

/// Build a validated analysis request for one package.
pub fn analysis_request(
    purl: &str,
    sha256: &str,
    analyzer_image: &str,
) -> Result<BuildRequest, RequestError> {
    let download_url =
        crates_io_download_url(purl).ok_or_else(|| RequestError::NotCratesIo(purl.to_string()))?;
    let request = BuildRequest {
        build_id: Uuid::new_v4(),
        source: SourceRef::RegistryArchive {
            purl: purl.to_string(),
            download_url,
            sha256: sha256.to_lowercase(),
        },
        pipeline_image: Some(analyzer_image.to_string()),
        requested_by: SERVICE_NAME.to_string(),
        requested_at: chrono::Utc::now(),
        target_registry: String::new(),
        metadata: HashMap::new(),
        // Empty: run the analyzer image's own entrypoint.
        command: vec![],
    };
    request.validate()?;
    Ok(request)
}

// ── Dedup window ─────────────────────────────────────────────────────────────

/// Packages requested recently, to bridge the gap before the graph shows the
/// run. Losing this on restart only risks one duplicate request per package.
pub struct RecentRequests {
    ttl: Duration,
    requested_at: HashMap<String, Instant>,
}

impl RecentRequests {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            requested_at: HashMap::new(),
        }
    }

    /// True — and records the request — if `purl` was not requested within the TTL.
    pub fn admit(&mut self, purl: &str, now: Instant) -> bool {
        self.requested_at
            .retain(|_, at| now.duration_since(*at) < self.ttl);
        if self.requested_at.contains_key(purl) {
            return false;
        }
        self.requested_at.insert(purl.to_string(), now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE: &str = "reg/polar-analyzer-cargo-manifest@sha256:abc";
    const SHA: &str = "8f42a60cbdf9a97f5d2305f08a87dc4e09308d1276d28c869c684d7777685682";

    #[test]
    fn download_url_for_plain_crates_io_purl() {
        assert_eq!(
            crates_io_download_url("pkg:cargo/itoa@1.0.18").as_deref(),
            Some("https://static.crates.io/crates/itoa/itoa-1.0.18.crate")
        );
        assert_eq!(
            crates_io_download_url("pkg:cargo/openssl-sys@0.9.117+patch").as_deref(),
            Some("https://static.crates.io/crates/openssl-sys/openssl-sys-0.9.117+patch.crate")
        );
    }

    #[test]
    fn no_download_url_for_qualified_or_foreign_purls() {
        for purl in [
            "pkg:cargo/sbomprobe@0.1.0?download_url=file://.",
            "pkg:cargo/foo@1.0.0?vcs_url=git%2Bhttps://example.com/foo",
            "pkg:npm/left-pad@1.3.0",
            "pkg:cargo/no-version",
            "pkg:cargo/../evil@1.0.0",
        ] {
            assert_eq!(crates_io_download_url(purl), None, "{purl}");
        }
    }

    #[test]
    fn analysis_request_is_valid_and_runs_image_entrypoint() {
        let r = analysis_request("pkg:cargo/itoa@1.0.18", &SHA.to_uppercase(), IMAGE).unwrap();
        assert_eq!(r.validate(), Ok(()));
        assert!(r.command.is_empty());
        assert_eq!(r.pipeline_image.as_deref(), Some(IMAGE));
        let SourceRef::RegistryArchive { sha256, .. } = &r.source else {
            panic!("expected archive source");
        };
        assert_eq!(sha256, SHA, "digest is normalized to lowercase");
    }

    #[test]
    fn analysis_request_round_trips_as_orchestrator_json() {
        let r = analysis_request("pkg:cargo/itoa@1.0.18", SHA, IMAGE).unwrap();
        let json = serde_json::to_vec(&r).unwrap();
        let back: BuildRequest = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.source, r.source);
        assert_eq!(back.validate(), Ok(()));
    }

    #[test]
    fn analysis_request_rejects_bad_digest() {
        assert!(matches!(
            analysis_request("pkg:cargo/itoa@1.0.18", "nope", IMAGE),
            Err(RequestError::Invalid(_))
        ));
    }

    #[test]
    fn requests_fill_only_free_in_flight_slots() {
        assert_eq!(requests_allowed(10, 20, 0), 10);
        assert_eq!(requests_allowed(10, 20, 15), 5);
        assert_eq!(requests_allowed(10, 20, 20), 0);
        assert_eq!(requests_allowed(10, 20, 25), 0, "never negative");
    }

    #[test]
    fn recent_requests_suppress_within_ttl_only() {
        let mut recent = RecentRequests::new(Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(recent.admit("pkg:cargo/itoa@1.0.18", t0));
        assert!(!recent.admit("pkg:cargo/itoa@1.0.18", t0 + Duration::from_secs(30)));
        assert!(recent.admit("pkg:cargo/itoa@1.0.18", t0 + Duration::from_secs(61)));
    }

    #[test]
    fn config_requires_pinned_image_and_defaults_the_rest() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            SchedulerConfig::from_lookup(env(&[])),
            Err(ConfigError::MissingAnalyzerImage)
        );
        assert!(matches!(
            SchedulerConfig::from_lookup(env(&[("ANALYZER_IMAGE", "reg/a:latest")])),
            Err(ConfigError::UnpinnedAnalyzerImage(_))
        ));
        let c = SchedulerConfig::from_lookup(env(&[("ANALYZER_IMAGE", IMAGE)])).unwrap();
        assert_eq!((c.batch_size, c.max_failed_attempts), (10, 3));
        assert!(matches!(
            SchedulerConfig::from_lookup(env(&[
                ("ANALYZER_IMAGE", IMAGE),
                ("ANALYSIS_BATCH_SIZE", "0")
            ])),
            Err(ConfigError::InvalidNumber("ANALYSIS_BATCH_SIZE", _))
        ));
    }
}
