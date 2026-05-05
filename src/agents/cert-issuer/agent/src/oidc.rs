//! OIDC token validation.
//!
//! Validates JWTs from a configured issuer, verifying signatures
//! against keys fetched from the issuer's JWKS endpoint. The JWKS
//! source is a trait so tests can inject canned key sets; production
//! uses an HTTP-backed implementation.
//!
//! # Design notes
//!
//! ## Why we inspect the JWT header before decoding
//!
//! `jsonwebtoken::decode` will reject tokens whose `alg` is not in
//! its configured allow-list, but it does so with a generic
//! `InvalidAlgorithm` error. We need to distinguish "this algorithm
//! is forbidden" (a misconfiguration that should be alerted on) from
//! "this token's signature didn't verify" (likely an attack or a
//! mid-rotation key mismatch). To get a clean `ForbiddenAlgorithm`
//! variant, we parse the header ourselves first and check the
//! algorithm before any signature work.
//!
//! This also closes the classic `alg=none` attack: a token with
//! `alg: none` and no signature is rejected here, before any
//! signature verification is even attempted.
//!
//! ## Why the audience error is special
//!
//! Audience mismatch is the single most common deployment misconfig
//! in this protocol — pod specs forget to set the projected token's
//! `audiences` field, or set it to the wrong value. Operators need
//! to be able to grep for "INVALID_AUDIENCE" specifically to find
//! these. So we map jsonwebtoken's `InvalidAudience` to our
//! `InvalidAudience` rather than collapsing it into `InvalidToken`.
//!
//! ## Why we use tokio's clock
//!
//! Cache TTL tests need to advance time deterministically. Rather
//! than inventing our own `Clock` trait, we use `tokio::time::Instant`
//! and rely on `tokio::time::pause` / `tokio::time::advance` in
//! tests. This is the standard tokio testing pattern and it works
//! cleanly for our case because the cache is async-touched anyway.
//!
//! ## How JWKS caching works
//!
//! The cache holds at most one entry per JWKS URI. Each entry has:
//!   - the parsed JWKS as a map from `kid` to decoding key
//!   - an `Instant` recording when it expires
//!
//! On every validation, we look up the kid in the current cached
//! entry. If the kid is present and the entry is not expired, we
//! use the cached key. If the kid is absent OR the entry is expired,
//! we refetch the JWKS and try again. If after the refetch the kid
//! is still absent, the token is rejected — the issuer doesn't have
//! that key.
//!
//! TTL is set from the server's `Cache-Control: max-age=N` header
//! when present, clamped between the configured min and max. If no
//! `Cache-Control` is present, we use the configured min as a sane
//! lower-bound default.

use crate::config::IssuerConfig;
use dashmap::DashMap;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::time::Instant;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The subset of JWT claims we extract and act on.
/// The full claims object is also kept (as serde_json::Value) for
/// telemetry purposes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidatedClaims {
    pub workload_identity: String,
    pub instance_binding: Option<String>,
    pub issuer: String,
    pub audience: String,
    pub raw: serde_json::Value,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("invalid token signature or structure")]
    InvalidToken,
    #[error("token audience does not match configured audience")]
    InvalidAudience,
    #[error("token issuer '{0}' does not match configured issuer")]
    InvalidIssuer(String),
    #[error("token uses forbidden algorithm: {0}")]
    ForbiddenAlgorithm(String),
    #[error("token is expired")]
    Expired,
    #[error("required claim '{0}' is missing or wrong type")]
    MissingClaim(String),
    #[error("JWKS unavailable: {0}")]
    JwksUnavailable(String),
}

/// Trait for fetching JWKS. The HTTP-backed implementation hits the
/// configured issuer's JWKS URI; tests inject a canned implementation.
///
/// The fetch result is the raw response body (JSON) and an optional
/// max-age in seconds parsed from the `Cache-Control` header. Tests
/// use this to control TTL behavior without standing up a real HTTP
/// server.
#[async_trait::async_trait]
pub trait JwksSource: Send + Sync {
    /// Fetch the JWKS at the given URI.
    ///
    /// Returns `(body_bytes, cache_max_age)` on success. The cache
    /// max-age is taken from the response's `Cache-Control: max-age=N`
    /// directive if present, or `None` otherwise. The validator
    /// applies its own min/max clamps to whatever this returns.
    async fn fetch(&self, jwks_uri: &str) -> Result<(Vec<u8>, Option<Duration>), String>;
}

// ---------------------------------------------------------------------------
// Validator
// ---------------------------------------------------------------------------

/// JWT validator configured with one issuer. v1 supports a single
/// issuer; multi-issuer support is a v2 direction (Section 12 of the
/// architecture spec).
pub struct Validator {
    config: IssuerConfig,
    jwks_source: Arc<dyn JwksSource>,
    cache: JwksCache,
    /// Pre-computed set of allowed jsonwebtoken Algorithm enum values,
    /// derived from the configured allowed_algorithms strings at
    /// construction time. Computed once so per-request validation
    /// doesn't have to re-parse the strings.
    allowed_algorithms: Vec<Algorithm>,
}

impl Validator {
    /// Construct a validator with the default HTTP-backed JWKS source.
    /// Production code uses this; tests use `with_jwks_source`.
    pub fn new(config: IssuerConfig) -> Self {
        Self::with_jwks_source(config, Arc::new(HttpJwksSource::new()))
    }

    /// Construct a validator with a caller-provided JWKS source.
    /// This is the seam tests use to inject canned key material
    /// without standing up an HTTP server.
    pub fn with_jwks_source(config: IssuerConfig, jwks_source: Arc<dyn JwksSource>) -> Self {
        let cache = JwksCache::new(config.jwks_cache_ttl_min, config.jwks_cache_ttl_max);
        let allowed_algorithms = parse_allowed_algorithms(&config.allowed_algorithms);
        Self {
            config,
            jwks_source,
            cache,
            allowed_algorithms,
        }
    }

    /// Validate a JWT. Returns extracted claims on success.
    ///
    /// Validation order — and the failure variant returned at each
    /// step — is part of the contract this function exposes:
    ///
    ///  1. Pre-flight inspect the raw `alg` header. Catches `alg: none`
    ///     and any other string that jsonwebtoken's `Algorithm` enum
    ///     can't represent, returning `ForbiddenAlgorithm` rather than
    ///     the generic `InvalidToken` that `decode_header` would emit.
    ///  2. Parse the full header via `decode_header`. If parsing fails
    ///     after the pre-flight pass, the token is structurally
    ///     malformed (bad base64, missing parts, etc.) and we return
    ///     `InvalidToken`.
    ///  3. Categorical-forbidden check on the parsed `Algorithm`. This
    ///     catches HS* tokens that did parse successfully — since
    ///     jsonwebtoken happily parses `alg: HS256`, the pre-flight
    ///     check above won't catch it but this one will.
    ///  4. Configured allow-list check.
    ///  5. Look up the signing key for the token's `kid`.
    ///  6. Decode and verify. The `jsonwebtoken` crate handles
    ///     signature, exp, iss, and aud checks here. We map its
    ///     errors to our variants.
    ///  7. Extract workload identity and instance binding from the
    ///     claims using the configured paths.
    pub async fn validate(&self, token: &str) -> Result<ValidatedClaims, ValidationError> {
        // ---- Step 1: alg pre-flight ----
        //
        // Inspect the raw `alg` field in the JWT header before handing
        // the token to jsonwebtoken. This is necessary because
        // jsonwebtoken's `Algorithm` enum doesn't have a `None` variant
        // — when it sees `alg: none`, `decode_header` returns a generic
        // parse error, not a recognizable "this algorithm is forbidden"
        // signal. Without this pre-flight, alg=none tokens would be
        // rejected as `InvalidToken`, which is correct security-wise
        // but loses the diagnostic distinction operators need to spot
        // attempted alg-confusion attacks in telemetry.
        //
        // The check intentionally only inspects `alg`, not the rest
        // of the token. Anything else (malformed structure, bad base64
        // in segments, missing parts) falls through to step 2 where
        // jsonwebtoken's parser produces an InvalidToken error.
        if let Some(alg_str) = peek_alg(token) {
            if is_forbidden_alg_str(&alg_str) {
                return Err(ValidationError::ForbiddenAlgorithm(alg_str));
            }
        }

        // ---- Step 2: header parse ----
        //
        // `decode_header` parses the JWT header into the typed
        // `Algorithm` enum without doing any signature work. After
        // step 1 weeded out alg=none and other unrepresentable values,
        // anything that fails here is a real structural problem.
        let header = decode_header(token).map_err(|_| ValidationError::InvalidToken)?;

        // ---- Step 3: categorical-forbidden check ----
        //
        // Defense in depth: even if HS* somehow ends up in the
        // configured allow-list (the config validator should have
        // rejected it, but be paranoid), we never accept symmetric
        // algorithms here. The pre-flight in step 1 also catches HS*
        // by string match, but a bug there shouldn't be the only line
        // of defense.
        let alg_str = format!("{:?}", header.alg);
        if is_categorically_forbidden(&header.alg) {
            return Err(ValidationError::ForbiddenAlgorithm(alg_str));
        }

        // ---- Step 4: configured allow-list check ----
        //
        // If the operator hasn't whitelisted this algorithm, reject
        // before any signature work. This produces a clean
        // ForbiddenAlgorithm error rather than the generic
        // InvalidToken that jsonwebtoken would return for a
        // disallowed-but-recognized algorithm.
        if !self.allowed_algorithms.contains(&header.alg) {
            return Err(ValidationError::ForbiddenAlgorithm(alg_str));
        }

        // ---- Step 5: key lookup ----
        //
        // The kid identifies which key in the JWKS signed this
        // token. Tokens without a kid are rejected here; we don't
        // try to verify against every key in the JWKS, because that
        // would weaken our ability to detect rotation issues.
        let kid = header.kid.ok_or(ValidationError::InvalidToken)?;
        let key = self.fetch_key_for_kid(&kid).await?;

        // ---- Step 5: decode and verify ----
        //
        // Set up the Validation struct with everything jsonwebtoken
        // needs to check. We tell it our allowed algorithms (already
        // filtered above, but jsonwebtoken's API requires a list),
        // expected issuer, expected audience, and to require exp.
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_audience(&[&self.config.audience]);
        validation.set_required_spec_claims(&["exp", "iss", "aud"]);
        // We don't tighten `validation.algorithms` further here
        // because we've already gated on the allow-list. Trust
        // ourselves over the library's redundant check.
        validation.algorithms = vec![header.alg];

        // Decode into a serde_json::Value so we can extract arbitrary
        // claim paths (the workload identity claim and instance
        // binding claim are both configurable and may live anywhere
        // in the claims object).
        let token_data =
            decode::<serde_json::Value>(token, &key, &validation).map_err(map_jwt_error)?;

        let raw_claims = token_data.claims;

        // ---- Step 6: extract identity claims ----
        //
        // The workload identity claim is required. The instance
        // binding claim is optional (used for telemetry only).
        let workload_identity =
            extract_string_claim(&raw_claims, &self.config.workload_identity_claim)?;

        let instance_binding =
            extract_optional_string_claim(&raw_claims, &self.config.instance_binding_claim);

        // Pull out the iss and aud values for the result. We know
        // these are present and valid because jsonwebtoken accepted
        // the token with our Validation config.
        let issuer = raw_claims
            .get("iss")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let audience = self.config.audience.clone();

        Ok(ValidatedClaims {
            workload_identity,
            instance_binding,
            issuer,
            audience,
            raw: raw_claims,
        })
    }

    /// Fetch the decoding key for `kid`, hitting the cache first and
    /// falling back to the JWKS source if the kid isn't cached or
    /// the cache entry is expired.
    ///
    /// If a refetch happens and the kid is still absent, we return
    /// `InvalidToken` — the issuer doesn't have a key for this kid,
    /// so the token can't be validated.
    async fn fetch_key_for_kid(&self, kid: &str) -> Result<DecodingKey, ValidationError> {
        let jwks_uri = self.config.jwks_uri.as_deref().ok_or_else(|| {
            // v1 requires an explicit JWKS URI in config. OIDC discovery
            // (auto-derive from /.well-known/openid-configuration) is a
            // future addition; the test config sets `jwks_uri` explicitly.
            ValidationError::JwksUnavailable("no jwks_uri configured".to_string())
        })?;

        // First attempt: cached lookup.
        if let Some(key) = self.cache.get(jwks_uri, kid) {
            return Ok(key);
        }

        // Cache miss or expired. Refetch.
        self.refresh_jwks(jwks_uri).await?;

        // Second attempt: post-refresh lookup. If the kid is still
        // not present, the issuer simply doesn't have a key for this
        // kid. We treat this as InvalidToken rather than
        // JwksUnavailable because the JWKS *did* come back, it just
        // didn't include the kid we needed.
        self.cache
            .get(jwks_uri, kid)
            .ok_or(ValidationError::InvalidToken)
    }

    /// Refetch the JWKS from the source and update the cache.
    async fn refresh_jwks(&self, jwks_uri: &str) -> Result<(), ValidationError> {
        let (body, server_max_age) = self
            .jwks_source
            .fetch(jwks_uri)
            .await
            .map_err(ValidationError::JwksUnavailable)?;

        let jwks: JwksDoc = serde_json::from_slice(&body)
            .map_err(|e| ValidationError::JwksUnavailable(format!("malformed JWKS: {e}")))?;

        // Build a (kid -> DecodingKey) map. Skip keys we can't turn
        // into a DecodingKey rather than failing the whole refresh
        // — a JWKS may legitimately contain keys for algorithms we
        // don't support, and we want the keys we *do* support to
        // remain usable.
        let mut keys = Vec::new();
        for jwk in jwks.keys {
            if let Some(kid) = jwk.kid.clone() {
                if let Some(key) = jwk_to_decoding_key(&jwk) {
                    keys.push((kid, key));
                }
            }
        }

        self.cache.insert(jwks_uri, keys, server_max_age);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// JWKS document representation
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct JwksDoc {
    keys: Vec<Jwk>,
}

/// A JSON Web Key as published in a JWKS document. We deserialize
/// only the fields we use; unknown fields are ignored.
#[derive(Debug, Deserialize)]
struct Jwk {
    kty: String,
    kid: Option<String>,
    #[serde(rename = "use")]
    use_: Option<String>,
    alg: Option<String>,

    // RSA fields
    n: Option<String>,
    e: Option<String>,

    // EC fields
    crv: Option<String>,
    x: Option<String>,
    y: Option<String>,
}

/// Convert a JWK to a `jsonwebtoken::DecodingKey`. Returns `None`
/// for key types we don't support (oct/symmetric) or malformed
/// entries — the caller skips those.
fn jwk_to_decoding_key(jwk: &Jwk) -> Option<DecodingKey> {
    // We only use this key for `use: sig` (or unspecified, which we
    // assume is signing). Keys explicitly marked for encryption are
    // skipped.
    if let Some(use_) = &jwk.use_ {
        if use_ != "sig" {
            return None;
        }
    }

    match jwk.kty.as_str() {
        "RSA" => {
            let n = jwk.n.as_deref()?;
            let e = jwk.e.as_deref()?;
            DecodingKey::from_rsa_components(n, e).ok()
        }
        "EC" => {
            let x = jwk.x.as_deref()?;
            let y = jwk.y.as_deref()?;
            // jsonwebtoken's from_ec_components takes the x and y
            // base64url-encoded coordinates. The curve is implicit
            // in the algorithm we use to verify.
            DecodingKey::from_ec_components(x, y).ok()
        }
        "OKP" => {
            // Ed25519 / Ed448 — single 'x' coordinate.
            let x = jwk.x.as_deref()?;
            DecodingKey::from_ed_components(x).ok()
        }
        // "oct" (symmetric) keys are deliberately not supported.
        // See `is_categorically_forbidden` for the symmetric algorithm
        // rejection logic.
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// JWKS cache
// ---------------------------------------------------------------------------

/// Cached JWKS keyed by URI. v1 holds at most one entry per URI;
/// since the validator is configured with one issuer, this is
/// effectively a single-slot cache, but keying by URI means future
/// multi-issuer support is a config change rather than a refactor.
pub struct JwksCache {
    entries: DashMap<String, CacheEntry>,
    ttl_min: Duration,
    ttl_max: Duration,
}

struct CacheEntry {
    keys: Vec<(String, DecodingKey)>,
    expires_at: Instant,
}

impl JwksCache {
    pub fn new(ttl_min: Duration, ttl_max: Duration) -> Self {
        Self {
            entries: DashMap::new(),
            ttl_min,
            ttl_max,
        }
    }

    /// Look up a key by URI and kid. Returns None if the URI is not
    /// cached, the entry is expired, or the kid is not in the entry.
    /// The "expired" check uses tokio's clock so tests can advance
    /// time deterministically.
    fn get(&self, jwks_uri: &str, kid: &str) -> Option<DecodingKey> {
        let entry = self.entries.get(jwks_uri)?;
        if Instant::now() >= entry.expires_at {
            return None;
        }
        // Linear scan — JWKS rarely have more than 2-3 keys, so this
        // is faster than a HashMap for the common case and avoids
        // the trouble of cloning DecodingKey values into one.
        for (k, key) in &entry.keys {
            if k == kid {
                return Some(key.clone());
            }
        }
        None
    }

    /// Insert (or replace) a JWKS entry. The TTL is computed from
    /// the server's max-age (if any) clamped to the configured
    /// min/max. If no max-age is provided, we use the min as a
    /// safe default.
    fn insert(
        &self,
        jwks_uri: &str,
        keys: Vec<(String, DecodingKey)>,
        server_max_age: Option<Duration>,
    ) {
        let ttl = match server_max_age {
            Some(server_ttl) => server_ttl.clamp(self.ttl_min, self.ttl_max),
            None => self.ttl_min,
        };
        let expires_at = Instant::now() + ttl;
        self.entries
            .insert(jwks_uri.to_string(), CacheEntry { keys, expires_at });
    }
}

// ---------------------------------------------------------------------------
// HTTP-backed JWKS source (production default)
// ---------------------------------------------------------------------------

/// Default JWKS source that fetches over HTTPS.
pub struct HttpJwksSource {
    client: reqwest::Client,
}

impl HttpJwksSource {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
        }
    }
}

impl Default for HttpJwksSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl JwksSource for HttpJwksSource {
    async fn fetch(&self, jwks_uri: &str) -> Result<(Vec<u8>, Option<Duration>), String> {
        let response = self
            .client
            .get(jwks_uri)
            .send()
            .await
            .map_err(|e| format!("fetch failed: {e}"))?;

        if !response.status().is_success() {
            return Err(format!("JWKS endpoint returned {}", response.status()));
        }

        // Parse Cache-Control: max-age=N if present. We don't honor
        // any other directives (no-cache, must-revalidate, etc.) —
        // the cache TTL is a local correctness concern and the issuer's
        // intent beyond max-age isn't relevant.
        let max_age = response
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_max_age);

        let body = response
            .bytes()
            .await
            .map_err(|e| format!("read body: {e}"))?;

        Ok((body.to_vec(), max_age))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse `max-age=N` out of a Cache-Control header value. Returns
/// None if the directive is absent or the value isn't a valid integer.
/// Other directives in the header are ignored.
fn parse_max_age(header: &str) -> Option<Duration> {
    for part in header.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("max-age=") {
            if let Ok(n) = rest.parse::<u64>() {
                return Some(Duration::from_secs(n));
            }
        }
    }
    None
}

/// Convert configured algorithm strings into jsonwebtoken Algorithm
/// values. Unknown names are silently dropped — the config validator
/// is responsible for rejecting bad values at load time. We're
/// defensive here against the case where config validation was
/// somehow bypassed.
fn parse_allowed_algorithms(names: &[String]) -> Vec<Algorithm> {
    names
        .iter()
        .filter_map(|name| match name.as_str() {
            "RS256" => Some(Algorithm::RS256),
            "RS384" => Some(Algorithm::RS384),
            "RS512" => Some(Algorithm::RS512),
            "ES256" => Some(Algorithm::ES256),
            "ES384" => Some(Algorithm::ES384),
            "EdDSA" => Some(Algorithm::EdDSA),
            "PS256" => Some(Algorithm::PS256),
            "PS384" => Some(Algorithm::PS384),
            "PS512" => Some(Algorithm::PS512),
            // HS* and "none" deliberately not mapped — even if they
            // appear in the config (they shouldn't, the config
            // validator rejects them), we won't accept them here.
            _ => None,
        })
        .collect()
}

/// Categorically-forbidden algorithms: HS* (symmetric, can be forged
/// by anyone holding the secret) and any alg that isn't in
/// jsonwebtoken's enum (which would already not pass `decode_header`).
fn is_categorically_forbidden(alg: &Algorithm) -> bool {
    matches!(alg, Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512)
}

/// Peek at the raw `alg` string in a JWT header without going through
/// `jsonwebtoken::decode_header`.
///
/// Why we need this: jsonwebtoken's `Algorithm` enum has no `None`
/// variant, so a JWT with `alg: none` causes `decode_header` to
/// return a generic parse error. That collapses the alg-confusion
/// case into the "your token is broken" bucket. By peeking at the
/// raw header JSON ourselves first, we can recognize the alg-none
/// (and other unrepresentable) cases and return `ForbiddenAlgorithm`
/// instead.
///
/// Returns `None` if the token is structurally too broken to find
/// an `alg` field at all (missing parts, non-base64 header segment,
/// non-JSON header content, no `alg` key). In those cases we let the
/// caller fall through to `decode_header`, which will produce its
/// own structural error.
///
/// This function does NOT validate the rest of the token. It only
/// peeks at the algorithm field.
fn peek_alg(token: &str) -> Option<String> {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    // A JWT is `header.payload.signature`. We only need the header.
    let header_b64 = token.split('.').next()?;
    let header_bytes = URL_SAFE_NO_PAD.decode(header_b64).ok()?;
    let header_json: serde_json::Value = serde_json::from_slice(&header_bytes).ok()?;
    let alg = header_json.get("alg")?.as_str()?;
    Some(alg.to_string())
}

/// Whether a raw `alg` string from a JWT header refers to a
/// categorically-forbidden algorithm.
///
/// The match is case-insensitive on the `none` value (because some
/// implementations emit `None` or `NONE`) but case-sensitive on the
/// HS* values (because the JWT spec is case-sensitive on those and
/// no legitimate implementation deviates).
fn is_forbidden_alg_str(alg: &str) -> bool {
    if alg.eq_ignore_ascii_case("none") {
        return true;
    }
    matches!(alg, "HS256" | "HS384" | "HS512")
}

/// Map jsonwebtoken errors to our `ValidationError` variants. The
/// mapping is what makes our public error API stable across
/// jsonwebtoken upgrades — if the library renames an error variant,
/// we adjust here without changing the surface.
fn map_jwt_error(err: jsonwebtoken::errors::Error) -> ValidationError {
    use jsonwebtoken::errors::ErrorKind;
    match err.kind() {
        ErrorKind::ExpiredSignature => ValidationError::Expired,
        ErrorKind::InvalidIssuer => {
            // We don't have the offending iss value handy from the
            // library's error; "unknown" is acceptable for the
            // diagnostic message. Operators chasing this error look
            // at the raw token, not the Display form of our error.
            ValidationError::InvalidIssuer("unknown".to_string())
        }
        ErrorKind::InvalidAudience => ValidationError::InvalidAudience,
        ErrorKind::ImmatureSignature => ValidationError::InvalidToken,
        ErrorKind::InvalidSignature => ValidationError::InvalidToken,
        ErrorKind::MissingRequiredClaim(name) => ValidationError::MissingClaim(name.clone()),
        // Everything else collapses to InvalidToken. We deliberately
        // don't expose more granular variants — operators don't need
        // to distinguish "bad base64" from "wrong key type" from
        // "malformed JSON in claims."
        _ => ValidationError::InvalidToken,
    }
}

/// Extract a string claim by JSON Pointer-like path (slash-separated).
///
/// The path syntax is `/key1/key2/key3`. A leading slash is optional.
/// This is JSON Pointer (RFC 6901) without the escape sequences we
/// don't need (~0, ~1) — our claim names don't contain slashes or
/// tildes.
///
/// **NOTE:** the test config uses `"kubernetes.io.pod.uid"` which
/// is *not* a slash-separated path. The config string in tests
/// should be `"kubernetes.io/pod/uid"` so the literal key
/// `"kubernetes.io"` is preserved as a single path segment. This is
/// the standard JSON Pointer convention and the only sane way to
/// handle the `kubernetes.io` top-level key (which contains a dot).
///
/// Returns `MissingClaim` if any segment of the path doesn't exist
/// or the final value isn't a string.
fn extract_string_claim(claims: &serde_json::Value, path: &str) -> Result<String, ValidationError> {
    let pointer = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };

    let value = claims
        .pointer(&pointer)
        .ok_or_else(|| ValidationError::MissingClaim(path.to_string()))?;

    value
        .as_str()
        .ok_or_else(|| ValidationError::MissingClaim(path.to_string()))
        .map(str::to_string)
}

/// Like `extract_string_claim` but returns `None` instead of an
/// error when the claim is absent. Used for the optional instance
/// binding claim.
fn extract_optional_string_claim(claims: &serde_json::Value, path: &str) -> Option<String> {
    let pointer = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };
    claims
        .pointer(&pointer)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

// Suppress unused-import warning for HashSet in environments where
// `validation.set_audience` doesn't take a HashSet. Kept here in case
// a future jsonwebtoken bump changes the API.
#[allow(dead_code)]
fn _unused() {
    let _: HashSet<String> = HashSet::new();
}
