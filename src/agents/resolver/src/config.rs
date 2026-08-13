//! Configuration for the OCI resolver.
//!
//! The shape of this file is defined in `config/schema.dhall` and lowered to a
//! flat YAML projection by `config/render.dhall`. The types in the `wire`
//! module mirror that projection one-for-one; `ResolverConfig` is the validated
//! domain representation that the rest of the resolver actually sees.
//!
//! The split exists because YAML has no sum types. Dhall makes invalid states
//! unrepresentable at authoring time, but `dhall-to-yaml` erases union tags on
//! the way out, so the discriminator is emitted as a plain string field and
//! reconstructed here. Everything past `ResolverConfig::try_from` deals in
//! enums, not in "this string field is meaningful only when that other string
//! field has a particular value".
//!
//! Requires `oci-client >= 0.16` for `ClientConfig::tls_certs_only`.

use oci_client::{
    Reference,
    client::{Certificate, CertificateEncoding, ClientConfig, ClientProtocol},
};
use serde::Deserialize;
use std::{path::PathBuf, str::FromStr, time::Duration};
use thiserror::Error;
use tracing::{debug, info, warn};

/// Environment variable naming the YAML config file. When unset the schema
/// defaults are used; when set but unreadable, startup fails rather than
/// silently falling back — a resolver running with different transport policy
/// than the operator believes is worse than a resolver that refuses to start.
pub const CONFIG_ENV: &str = "POLAR_OCI_RESOLVER_CONFIG";

/// The rendered schema defaults, embedded so that `Default` cannot drift from
/// `schema.dhall`. Regenerate with:
///   dhall-to-yaml --preserve-null --file config/defaults-to-yaml.dhall \
///     > config/default.yaml
/// CI re-renders and runs `git diff --exit-code` on the result.
pub const DEFAULT_CONFIG_YAML: &str = include_str!("../config/default.yaml");

/// Registry hostname that `Reference::resolve_registry()` rewrites `docker.io`
/// to. Anything keyed on the post-resolution host — the plaintext exception
/// list, the credential lookup — must use this spelling, not `docker.io`.
pub const DOCKER_HUB_RESOLVED_HOST: &str = "index.docker.io";
pub const DOCKER_HUB_CANONICAL_HOST: &str = "docker.io";
/// The key Docker itself writes into `~/.docker/config.json` for Hub.
pub const DOCKER_HUB_CREDENTIAL_KEY: &str = "https://index.docker.io/v1/";

// ---------------------------------------------------------------------------
// Wire types — a literal mirror of the YAML that `render.dhall` emits.
// ---------------------------------------------------------------------------

pub mod wire {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct Config {
        #[serde(default)]
        pub registry: Registry,
        #[serde(default)]
        pub tls: Tls,
        #[serde(default)]
        pub auth: Auth,
        #[serde(default)]
        pub http: Http,
        #[serde(default)]
        pub mirrors: Vec<Mirror>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct Registry {
        /// `"skip"` or `"qualify"`.
        pub unqualified_refs: String,
        pub default_registry: Option<String>,
        pub default_namespace: Option<String>,
    }

    impl Default for Registry {
        fn default() -> Self {
            Self {
                unqualified_refs: "qualify".into(),
                default_registry: Some(DOCKER_HUB_CANONICAL_HOST.into()),
                default_namespace: Some("library".into()),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct Tls {
        /// `"https"`, `"https-except"` or `"http-insecure"`.
        pub mode: String,
        #[serde(default)]
        pub plaintext_hosts: Vec<String>,
        #[serde(default)]
        pub extra_root_certificates: Vec<String>,
        #[serde(default)]
        pub exclusive_trust_store: bool,
    }

    impl Default for Tls {
        fn default() -> Self {
            Self {
                mode: "https".into(),
                plaintext_hosts: Vec::new(),
                extra_root_certificates: Vec::new(),
                exclusive_trust_store: false,
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct Auth {
        #[serde(default)]
        pub docker_config: Option<String>,
        pub anonymous_fallback: bool,
    }

    impl Default for Auth {
        fn default() -> Self {
            Self {
                docker_config: None,
                anonymous_fallback: true,
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct Http {
        #[serde(default)]
        pub connect_timeout_ms: Option<u64>,
        #[serde(default)]
        pub read_timeout_ms: Option<u64>,
        #[serde(default)]
        pub https_proxy: Option<String>,
        #[serde(default)]
        pub http_proxy: Option<String>,
        #[serde(default)]
        pub no_proxy: Option<String>,
        pub user_agent: String,
    }

    impl Default for Http {
        fn default() -> Self {
            Self {
                connect_timeout_ms: Some(5_000),
                read_timeout_ms: Some(30_000),
                https_proxy: None,
                http_proxy: None,
                no_proxy: None,
                user_agent: "polar-oci-resolver".into(),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct Mirror {
        pub upstream: String,
        pub mirror: String,
    }
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnqualifiedRefPolicy {
    /// Drop the event. Correct only where an egress attempt to a public
    /// registry is itself a finding — see cmu-sei/Polar#219.
    Skip,
    Qualify {
        registry: String,
        /// Inserted only for single-segment repositories.
        namespace: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportPolicy {
    Https,
    /// Plaintext for exactly these post-resolution `host[:port]` keys.
    HttpsExcept(Vec<String>),
    HttpInsecure,
}

impl TransportPolicy {
    /// Whether plaintext is permitted for a post-resolution registry host.
    /// Mirrors `ClientProtocol::scheme_for`, which is private upstream.
    pub fn permits_plaintext(&self, resolved_registry: &str) -> bool {
        match self {
            Self::Https => false,
            Self::HttpInsecure => true,
            Self::HttpsExcept(hosts) => hosts.iter().any(|h| h == resolved_registry),
        }
    }
}

impl From<&TransportPolicy> for ClientProtocol {
    fn from(p: &TransportPolicy) -> Self {
        match p {
            TransportPolicy::Https => ClientProtocol::Https,
            TransportPolicy::HttpInsecure => ClientProtocol::Http,
            TransportPolicy::HttpsExcept(hosts) => ClientProtocol::HttpsExcept(hosts.clone()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolverConfig {
    pub unqualified_refs: UnqualifiedRefPolicy,
    pub transport: TransportPolicy,
    pub extra_root_certificates: Vec<PathBuf>,
    pub exclusive_trust_store: bool,
    pub docker_config: Option<PathBuf>,
    pub anonymous_fallback: bool,
    pub connect_timeout: Option<Duration>,
    pub read_timeout: Option<Duration>,
    pub https_proxy: Option<String>,
    pub http_proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub user_agent: String,
    /// upstream registry -> mirror host.
    pub mirrors: Vec<(String, String)>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

impl ResolverConfig {
    /// Load from `$POLAR_OCI_RESOLVER_CONFIG`, or fall back to the embedded
    /// schema defaults. A set-but-broken path is fatal.
    pub fn load() -> Result<Self, ConfigError> {
        match std::env::var(CONFIG_ENV) {
            Ok(path) if !path.trim().is_empty() => {
                let path = PathBuf::from(path);
                info!("Loading OCI resolver configuration from {}", path.display());
                let raw = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
                    path: path.clone(),
                    source,
                })?;
                let wire: wire::Config =
                    serde_yaml::from_str(&raw).map_err(|source| ConfigError::Parse {
                        path: path.clone(),
                        source,
                    })?;
                Self::try_from(wire)
            }
            _ => {
                info!(
                    "{CONFIG_ENV} unset; using built-in defaults (qualify unqualified refs against \
                     {DOCKER_HUB_CANONICAL_HOST}, TLS for all registries)"
                );
                let wire: wire::Config =
                    serde_yaml::from_str(DEFAULT_CONFIG_YAML).map_err(|source| {
                        ConfigError::Parse {
                            path: PathBuf::from("<embedded default.yaml>"),
                            source,
                        }
                    })?;
                Self::try_from(wire)
            }
        }
    }

    /// Build the `oci-client` configuration.
    ///
    /// `extra` lets the caller append certificates discovered at runtime (the
    /// `PROXY_CA_CERT` lookup) without them replacing the configured ones —
    /// the previous implementation dropped the entire protocol setting on the
    /// branch where a proxy CA was present, silently reverting a configured
    /// plaintext allowlist back to TLS-everywhere.
    pub fn client_config(&self, extra: Vec<Certificate>) -> Result<ClientConfig, ConfigError> {
        let mut certs = Vec::with_capacity(self.extra_root_certificates.len() + extra.len());
        for path in &self.extra_root_certificates {
            let data = std::fs::read(path).map_err(|source| ConfigError::Io {
                path: path.clone(),
                source,
            })?;
            certs.push(Certificate {
                encoding: CertificateEncoding::Pem,
                data,
            });
        }
        certs.extend(extra);

        debug!(
            "Building OCI client: transport={:?}, {} root certificate(s), exclusive_trust_store={}",
            self.transport,
            certs.len(),
            self.exclusive_trust_store
        );

        Ok(ClientConfig {
            protocol: ClientProtocol::from(&self.transport),
            extra_root_certificates: certs,
            connect_timeout: self.connect_timeout,
            read_timeout: self.read_timeout,
            https_proxy: self.https_proxy.clone(),
            http_proxy: self.http_proxy.clone(),
            no_proxy: self.no_proxy.clone(),
            // `ClientConfig::user_agent` is `&'static str`. A single bounded
            // leak at startup is the honest cost of that API; the alternative
            // is a `OnceLock<String>`, which is the same leak with extra steps.
            user_agent: Box::leak(self.user_agent.clone().into_boxed_str()),
            ..ClientConfig::default()
        })
    }

    /// Apply a configured pull-through mirror, if one matches.
    ///
    /// NOTE: `Reference::set_mirror_registry` is `#[doc(hidden)]` upstream and
    /// explicitly exempt from semver. Pin `oci-client` with `=` if you enable
    /// mirrors in production.
    pub fn apply_mirror(&self, reference: &mut Reference) {
        if let Some((_, mirror)) = self
            .mirrors
            .iter()
            .find(|(upstream, _)| upstream == reference.registry())
        {
            debug!(
                "Routing {} via mirror {mirror} (ns={})",
                reference.registry(),
                reference.registry()
            );
            reference.set_mirror_registry(mirror.clone());
        }
    }

    /// Docker-config credential keys to try for a post-resolution registry
    /// host, in priority order.
    ///
    /// Docker Hub is special-cased because `resolve_registry()` yields
    /// `index.docker.io` while the credential store is keyed on the legacy
    /// `https://index.docker.io/v1/`. Without the alias, defaulting unqualified
    /// refs to Docker Hub would authenticate anonymously against a rate-limited
    /// endpoint even when valid credentials are on disk.
    ///
    /// `http://` is emitted only where the transport policy actually permits
    /// plaintext for this host, rather than unconditionally.
    pub fn credential_keys(&self, resolved_registry: &str) -> Vec<String> {
        let base = normalize_registry_host(resolved_registry);
        let mut keys = Vec::new();

        if base == DOCKER_HUB_RESOLVED_HOST || base == DOCKER_HUB_CANONICAL_HOST {
            keys.push(DOCKER_HUB_CREDENTIAL_KEY.to_string());
            keys.push(DOCKER_HUB_RESOLVED_HOST.to_string());
            keys.push(DOCKER_HUB_CANONICAL_HOST.to_string());
            keys.push("registry-1.docker.io".to_string());
        }

        keys.push(base.clone());
        keys.push(format!("https://{base}"));
        keys.push(format!("https://{base}/"));
        keys.push(format!("https://{base}/v1/"));
        keys.push(format!("{base}/v1/"));

        if self.transport.permits_plaintext(&base) {
            keys.push(format!("http://{base}"));
            keys.push(format!("http://{base}/v1/"));
        }

        // `Vec::dedup` only collapses *consecutive* duplicates, and the Docker
        // Hub alias block interleaves with the generic block.
        let mut seen = std::collections::HashSet::new();
        keys.retain(|k| seen.insert(k.clone()));
        keys
    }
}

pub fn normalize_registry_host(s: &str) -> String {
    s.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

impl TryFrom<wire::Config> for ResolverConfig {
    type Error = ConfigError;

    fn try_from(w: wire::Config) -> Result<Self, Self::Error> {
        let invalid = |m: String| ConfigError::Invalid(m);

        let unqualified_refs = match w.registry.unqualified_refs.as_str() {
            "skip" => {
                if w.registry.default_registry.is_some() || w.registry.default_namespace.is_some() {
                    return Err(invalid(
                        "registry.unqualifiedRefs is \"skip\" but defaultRegistry/defaultNamespace \
                         are set; these would be silently ignored"
                            .into(),
                    ));
                }
                UnqualifiedRefPolicy::Skip
            }
            "qualify" => {
                let registry = w.registry.default_registry.clone().ok_or_else(|| {
                    invalid(
                        "registry.unqualifiedRefs is \"qualify\" but defaultRegistry is null"
                            .into(),
                    )
                })?;
                validate_registry_host(&registry)?;
                UnqualifiedRefPolicy::Qualify {
                    registry,
                    namespace: w.registry.default_namespace.clone(),
                }
            }
            other => {
                return Err(invalid(format!(
                    "registry.unqualifiedRefs must be \"skip\" or \"qualify\", got {other:?}"
                )));
            }
        };

        let transport = match w.tls.mode.as_str() {
            "https" | "http-insecure" if !w.tls.plaintext_hosts.is_empty() => {
                return Err(invalid(format!(
                    "tls.plaintextHosts is non-empty but tls.mode is {:?}; the list would be \
                     ignored. Use mode \"https-except\".",
                    w.tls.mode
                )));
            }
            "https" => TransportPolicy::Https,
            "http-insecure" => {
                warn!(
                    "tls.mode is \"http-insecure\": ALL registry traffic, including credentials, \
                     will be sent in plaintext. This must not be used outside test fixtures."
                );
                TransportPolicy::HttpInsecure
            }
            "https-except" => {
                // explict check, we don't want to ever enable blanket HTTP
                // embodies least priviledge
                if w.tls.plaintext_hosts.is_empty() {
                    return Err(invalid(
                        "tls.mode is \"https-except\" but plaintextHosts is empty; use mode \
                         \"https\" to express TLS everywhere"
                            .into(),
                    ));
                }
                for host in &w.tls.plaintext_hosts {
                    validate_plaintext_host(host)?;
                    warn!("Plaintext transport enabled for registry {host}");
                }
                TransportPolicy::HttpsExcept(w.tls.plaintext_hosts.clone())
            }
            other => {
                return Err(invalid(format!(
                    "tls.mode must be \"https\", \"https-except\" or \"http-insecure\", got \
                     {other:?}"
                )));
            }
        };

        if w.tls.exclusive_trust_store && w.tls.extra_root_certificates.is_empty() {
            return Err(invalid(
                "tls.exclusiveTrustStore is true but extraRootCertificates is empty; the client \
                 would trust no roots at all"
                    .into(),
            ));
        }

        let mut mirrors = Vec::with_capacity(w.mirrors.len());
        for m in &w.mirrors {
            validate_registry_host(&m.upstream)?;
            validate_registry_host(&m.mirror)?;
            mirrors.push((m.upstream.clone(), m.mirror.clone()));
        }

        Ok(ResolverConfig {
            unqualified_refs,
            transport,
            extra_root_certificates: w
                .tls
                .extra_root_certificates
                .iter()
                .map(PathBuf::from)
                .collect(),
            exclusive_trust_store: w.tls.exclusive_trust_store,
            docker_config: w.auth.docker_config.map(PathBuf::from),
            anonymous_fallback: w.auth.anonymous_fallback,
            connect_timeout: w.http.connect_timeout_ms.map(Duration::from_millis),
            read_timeout: w.http.read_timeout_ms.map(Duration::from_millis),
            https_proxy: w.http.https_proxy,
            http_proxy: w.http.http_proxy,
            no_proxy: w.http.no_proxy,
            user_agent: w.http.user_agent,
            mirrors,
        })
    }
}

/// A registry host is valid iff parsing `<host>/probe` yields `<host>` back as
/// the registry. This reuses `oci-spec`'s vetted grammar instead of
/// reimplementing hostname rules, and rejects the common mistake of writing a
/// bare word (`myregistry`), which `split_domain` would silently reinterpret as
/// the first path segment of a Docker Hub repository.
fn validate_registry_host(host: &str) -> Result<(), ConfigError> {
    if host.contains("://") || host.contains('/') {
        return Err(ConfigError::Invalid(format!(
            "registry host {host:?} must be a bare host[:port], with no scheme or path"
        )));
    }
    let probe = format!("{host}/probe");
    let parsed = Reference::from_str(&probe).map_err(|e| {
        ConfigError::Invalid(format!("registry host {host:?} is not parseable: {e}"))
    })?;
    // `index.docker.io` legitimately normalises to `docker.io`; accept that.
    let ok = parsed.registry() == host
        || (host == DOCKER_HUB_RESOLVED_HOST && parsed.registry() == DOCKER_HUB_CANONICAL_HOST);
    if !ok {
        return Err(ConfigError::Invalid(format!(
            "registry host {host:?} is not recognised as a registry (it parsed as \
             registry={:?}); a registry host must contain a dot, carry an explicit port, or be \
             \"localhost\"",
            parsed.registry()
        )));
    }
    Ok(())
}

/// Entries in `plaintextHosts` are compared by exact string equality against
/// `Reference::resolve_registry()`. Anything that cannot possibly match is a
/// no-op, and a silent no-op on a security-relevant setting is worse than a
/// startup failure.
fn validate_plaintext_host(host: &str) -> Result<(), ConfigError> {
    if host.contains("://") || host.contains('/') {
        return Err(ConfigError::Invalid(format!(
            "tls.plaintextHosts entry {host:?} must be a bare host[:port]; it is matched verbatim \
             against Reference::resolve_registry() and a scheme or path can never match"
        )));
    }
    if host == DOCKER_HUB_CANONICAL_HOST {
        return Err(ConfigError::Invalid(format!(
            "tls.plaintextHosts entry {DOCKER_HUB_CANONICAL_HOST:?} can never match: \
             Reference::resolve_registry() rewrites it to {DOCKER_HUB_RESOLVED_HOST:?}. (You \
             almost certainly do not want plaintext Docker Hub regardless.)"
        )));
    }
    validate_registry_host(host)
}

// ---------------------------------------------------------------------------
// Reference qualification
// ---------------------------------------------------------------------------

/// Whether an image ref already carries a registry component.
///
/// This is a deliberate transcription of `oci-spec`'s `split_domain` predicate.
/// Any divergence means we and the parser disagree about what a ref means,
/// which is exactly the class of bug that produced duplicate registry nodes in
/// the graph. In particular `localhost/foo` *is* qualified — the previous
/// heuristic required a dot or a colon and therefore dropped it.
pub fn is_qualified(raw: &str) -> bool {
    match raw.split_once('/') {
        None => false,
        Some((left, _)) => left.contains('.') || left.contains(':') || left == "localhost",
    }
}

/// Split `name[:tag][@digest]`.
///
/// Only ever called on refs already known to be unqualified, which is what
/// makes the naive colon scan safe: a colon in the first path segment would
/// have made the ref qualified, so any remaining colon after the last slash is
/// a tag separator.
fn split_suffixes(raw: &str) -> (&str, Option<&str>, Option<&str>) {
    let (rest, digest) = match raw.split_once('@') {
        Some((r, d)) => (r, Some(d)),
        None => (raw, None),
    };
    let tag_sep = rest
        .rfind(':')
        .filter(|i| rest.rfind('/').map(|s| *i > s).unwrap_or(true));
    match tag_sep {
        Some(i) => (&rest[..i], Some(&rest[i + 1..]), digest),
        None => (rest, None, digest),
    }
}

/// Apply the configured policy and produce a `Reference`, or `None` when the
/// ref is unqualified and policy says to skip.
///
/// Qualification is performed on the *string*, before parsing, rather than by
/// mutating a parsed `Reference`. That is intentional: `oci-spec` hardcodes
/// `docker.io` and an implicit `library/` insertion inside `split_domain`, so
/// letting it parse first would make the configured namespace unreachable
/// whenever the configured registry happens to be Docker Hub.
pub fn qualify(
    raw: &str,
    policy: &UnqualifiedRefPolicy,
) -> Result<Option<Reference>, oci_client::ParseError> {
    if is_qualified(raw) {
        return Reference::from_str(raw).map(Some);
    }
    match policy {
        UnqualifiedRefPolicy::Skip => Ok(None),
        UnqualifiedRefPolicy::Qualify {
            registry,
            namespace,
        } => {
            let (name, tag, digest) = split_suffixes(raw);
            let name = match namespace {
                Some(ns) if !name.contains('/') => format!("{ns}/{name}"),
                _ => name.to_string(),
            };
            let mut qualified = format!("{registry}/{name}");
            if let Some(t) = tag {
                qualified.push(':');
                qualified.push_str(t);
            }
            if let Some(d) = digest {
                qualified.push('@');
                qualified.push_str(d);
            }
            debug!("Qualified unqualified image ref {raw:?} as {qualified:?}");
            Reference::from_str(&qualified).map(Some)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn hub() -> UnqualifiedRefPolicy {
        UnqualifiedRefPolicy::Qualify {
            registry: "docker.io".into(),
            namespace: Some("library".into()),
        }
    }

    /// The embedded defaults and the hand-written `Default` impls are two
    /// encodings of `schema.dhall`. This is the gate that keeps them equal.
    #[test]
    fn embedded_defaults_match_default_impl() {
        let parsed: wire::Config = serde_yaml::from_str(DEFAULT_CONFIG_YAML).unwrap();
        assert_eq!(parsed, wire::Config::default());
    }

    #[test]
    fn embedded_defaults_validate() {
        let parsed: wire::Config = serde_yaml::from_str(DEFAULT_CONFIG_YAML).unwrap();
        let cfg = ResolverConfig::try_from(parsed).unwrap();
        assert_eq!(cfg.unqualified_refs, hub());
        assert_eq!(cfg.transport, TransportPolicy::Https);
    }

    #[test]
    fn qualification_matches_docker_semantics() {
        let cases = [
            (
                "rancher/klipper-helm:v0.9.14-build20260210",
                "docker.io",
                "rancher/klipper-helm",
            ),
            ("polar-nu-init:latest", "docker.io", "library/polar-nu-init"),
            ("nginx", "docker.io", "library/nginx"),
            ("nginx:1.25.3", "docker.io", "library/nginx"),
            ("a/b/c/d:1", "docker.io", "a/b/c/d"),
            // already qualified — passed through untouched
            ("ghcr.io/cmu-sei/polar:0.1", "ghcr.io", "cmu-sei/polar"),
            ("localhost/foo:1", "localhost", "foo"),
            ("localhost:5000/foo:1", "localhost:5000", "foo"),
            // legacy spelling normalises, so the graph gets one registry node
            (
                "index.docker.io/library/redis:7",
                "docker.io",
                "library/redis",
            ),
        ];
        for (raw, registry, repository) in cases {
            let r = qualify(raw, &hub()).unwrap().expect("should resolve");
            assert_eq!(r.registry(), registry, "registry for {raw}");
            assert_eq!(r.repository(), repository, "repository for {raw}");
        }
    }

    #[test]
    fn qualification_preserves_tag_and_digest() {
        let r = qualify(&format!("nginx:1.25.3@{SHA}"), &hub())
            .unwrap()
            .unwrap();
        assert_eq!(r.repository(), "library/nginx");
        assert_eq!(r.tag(), Some("1.25.3"));
        assert_eq!(r.digest(), Some(SHA));

        let r = qualify(&format!("nginx@{SHA}"), &hub()).unwrap().unwrap();
        assert_eq!(r.tag(), None);
        assert_eq!(r.digest(), Some(SHA));
    }

    #[test]
    fn namespace_is_not_applied_to_multi_segment_repositories() {
        let policy = UnqualifiedRefPolicy::Qualify {
            registry: "harbor.corp.internal".into(),
            namespace: Some("library".into()),
        };
        let r = qualify("team/svc:1.0", &policy).unwrap().unwrap();
        assert_eq!(r.registry(), "harbor.corp.internal");
        assert_eq!(r.repository(), "team/svc");

        let r = qualify("svc:1.0", &policy).unwrap().unwrap();
        assert_eq!(r.repository(), "library/svc");
    }

    #[test]
    fn namespace_none_leaves_single_segment_repositories_alone() {
        let policy = UnqualifiedRefPolicy::Qualify {
            registry: "harbor.corp.internal".into(),
            namespace: None,
        };
        let r = qualify("svc:1.0", &policy).unwrap().unwrap();
        assert_eq!(r.repository(), "svc");
    }

    #[test]
    fn skip_policy_drops_unqualified_but_keeps_qualified() {
        assert!(
            qualify("polar-nu-init:latest", &UnqualifiedRefPolicy::Skip)
                .unwrap()
                .is_none()
        );
        assert!(
            qualify("ghcr.io/a/b:1", &UnqualifiedRefPolicy::Skip)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn docker_hub_credential_keys_include_the_legacy_v1_key() {
        let cfg = ResolverConfig::try_from(wire::Config::default()).unwrap();
        let keys = cfg.credential_keys("index.docker.io");
        assert!(keys.contains(&DOCKER_HUB_CREDENTIAL_KEY.to_string()));
        assert!(!keys.iter().any(|k| k.starts_with("http://")));
    }

    #[test]
    fn plaintext_keys_appear_only_for_allowlisted_hosts() {
        let mut w = wire::Config::default();
        w.tls.mode = "https-except".into();
        w.tls.plaintext_hosts = vec!["registry.local:5000".into()];
        let cfg = ResolverConfig::try_from(w).unwrap();

        assert!(
            cfg.credential_keys("registry.local:5000")
                .iter()
                .any(|k| k.starts_with("http://"))
        );
        assert!(
            !cfg.credential_keys("ghcr.io")
                .iter()
                .any(|k| k.starts_with("http://"))
        );
    }

    #[test]
    fn rejects_configurations_whose_fields_would_be_ignored() {
        let mut w = wire::Config::default();
        w.tls.plaintext_hosts = vec!["registry.local:5000".into()]; // mode is still "https"
        assert!(ResolverConfig::try_from(w).is_err());

        let mut w = wire::Config::default();
        w.tls.mode = "https-except".into();
        assert!(ResolverConfig::try_from(w).is_err()); // empty exception list

        let mut w = wire::Config::default();
        w.registry.unqualified_refs = "skip".into(); // registry fields still set
        assert!(ResolverConfig::try_from(w).is_err());

        let mut w = wire::Config::default();
        w.tls.exclusive_trust_store = true; // with no certificates
        assert!(ResolverConfig::try_from(w).is_err());
    }

    #[test]
    fn rejects_unmatchable_plaintext_hosts() {
        for host in [
            "docker.io",
            "http://registry.local:5000",
            "registry.local/v2",
        ] {
            let mut w = wire::Config::default();
            w.tls.mode = "https-except".into();
            w.tls.plaintext_hosts = vec![host.into()];
            assert!(
                ResolverConfig::try_from(w).is_err(),
                "expected {host} to be rejected"
            );
        }
    }

    #[test]
    fn rejects_bare_word_default_registry() {
        let mut w = wire::Config::default();
        w.registry.default_registry = Some("myregistry".into());
        assert!(ResolverConfig::try_from(w).is_err());

        let mut w = wire::Config::default();
        w.registry.default_registry = Some("harbor.corp.internal".into());
        assert!(ResolverConfig::try_from(w).is_ok());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let yaml = "registry:\n  unqualifiedRefs: qualify\n  defaultRegistry: docker.io\n  \
                    defaultNamespace: library\n  typo: true\n";
        assert!(serde_yaml::from_str::<wire::Config>(yaml).is_err());
    }
}
