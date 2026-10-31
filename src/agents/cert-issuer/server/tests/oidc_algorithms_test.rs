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

//! What the token validator (`src/oidc.rs`) accepts and refuses today, for every algorithm it maps, pinned before
//! jsonwebtoken's crypto backend is changed (remaining-work.md, "Left by the release advisory fixes"). The tests in
//! `oidc_test.rs` are all RS256; these sign with ES256, ES384, EdDSA, RS384/512 and PS256/384/512 as well, and refuse
//! what is not allowed, `alg: none` in every case, a key of one family used for another, an expired token, a wrong
//! audience and issuer, and read the `kubernetes.io` claims through each.
//!
//! The keys are made here: RSA by the shared helpers (`helpers.rs`), EC and Ed25519 by rcgen. Nothing is a fixture.
//!
//! What a backend change must keep, or change on purpose:
//!   - every name `parse_allowed_algorithms` maps verifies when its issuer allows it, and only then;
//!   - an algorithm the issuer does not allow is `ForbiddenAlgorithm`, named as `{:?}` of jsonwebtoken's `Algorithm`,
//!     before any JWKS fetch;
//!   - `none` in any case and HS256/384/512 are `ForbiddenAlgorithm` before any fetch, whatever the configuration;
//!     an `alg` jsonwebtoken cannot represent (ES512, lowercase hs256, ...) is `InvalidToken`, also before any fetch;
//!   - a key of the wrong family or curve for the token's `alg` is `InvalidToken`;
//!   - `exp` is required and checked with jsonwebtoken's default leeway of 60 seconds; `aud` and `iss` are required;
//!   - an `nbf` or an `iat` later than now by more than that leeway is `NotYetValid`, naming the claim;
//!   - a token that lives longer than the maximum (`exp - iat`, or `exp - now` without `iat`) is `LifetimeTooLong`;
//!     the maximum is a day unless the validator is given another (`CERT_ISSUER_MAX_TOKEN_LIFETIME_SECS`).

mod helpers;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use cert_issuer::config::IssuerConfig;
use cert_issuer::oidc::{DEFAULT_MAX_TOKEN_LIFETIME, ValidationError, Validator};
use cert_issuer_common::identity::IdentityNormalization;
use helpers::*;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rcgen::KeyPair;
use rsa::pkcs8::EncodePrivateKey;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Every algorithm `parse_allowed_algorithms` maps, by the name the configuration gives it.
const MAPPED: [(&str, Algorithm); 9] = [
    ("RS256", Algorithm::RS256),
    ("RS384", Algorithm::RS384),
    ("RS512", Algorithm::RS512),
    ("PS256", Algorithm::PS256),
    ("PS384", Algorithm::PS384),
    ("PS512", Algorithm::PS512),
    ("ES256", Algorithm::ES256),
    ("ES384", Algorithm::ES384),
    ("EdDSA", Algorithm::EdDSA),
];

/// What the exemplar targets allow for the cluster's own issuer (infra/targets/*/config.nuon).
const DEPLOYED: [&str; 3] = ["RS256", "ES256", "EdDSA"];

const POD_UID: &str = "11111111-2222-3333-4444-555555555555";
const SUBJECT: &str = "system:serviceaccount:polar:git-observer";

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// A private key that signs tokens, and the JWK an issuer would publish for its public half.
struct SigningKey {
    encoding: EncodingKey,
    jwk: serde_json::Value,
}

fn rsa_key(kid: &str) -> SigningKey {
    ensure_crypto_provider();
    let key = primary_keypair();
    let pem = key
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .expect("to_pkcs8_pem");
    SigningKey {
        encoding: EncodingKey::from_rsa_pem(pem.as_bytes()).expect("rsa encoding key"),
        // jwk_for writes `alg: RS256`; the validator does not read a JWK's `alg`, so the PS* tokens verify
        // against it too (pinned by every_mapped_algorithm_verifies_when_its_issuer_allows_it).
        jwk: jwk_for(kid, key),
    }
}

/// A P-256 or P-384 key, as rcgen makes it (PKCS#8), with its JWK from the uncompressed point (0x04 || x || y).
fn ec_key(kid: &str, curve: &'static rcgen::SignatureAlgorithm, crv: &str) -> SigningKey {
    ensure_crypto_provider();
    let pair = KeyPair::generate_for(curve).expect("ec keypair generation");
    let raw = pair.public_key_raw();
    assert_eq!(raw[0], 0x04, "an uncompressed point");
    let half = (raw.len() - 1) / 2;
    SigningKey {
        encoding: EncodingKey::from_ec_pem(pair.serialize_pem().as_bytes())
            .expect("ec encoding key"),
        jwk: serde_json::json!({
            "kty": "EC",
            "kid": kid,
            "use": "sig",
            "crv": crv,
            "x": URL_SAFE_NO_PAD.encode(&raw[1..1 + half]),
            "y": URL_SAFE_NO_PAD.encode(&raw[1 + half..]),
        }),
    }
}

fn p256_key(kid: &str) -> SigningKey {
    ec_key(kid, &rcgen::PKCS_ECDSA_P256_SHA256, "P-256")
}

fn p384_key(kid: &str) -> SigningKey {
    ec_key(kid, &rcgen::PKCS_ECDSA_P384_SHA384, "P-384")
}

fn ed25519_key(kid: &str) -> SigningKey {
    ensure_crypto_provider();
    let pair = KeyPair::generate_for(&rcgen::PKCS_ED25519).expect("ed25519 keypair generation");
    SigningKey {
        encoding: EncodingKey::from_ed_pem(pair.serialize_pem().as_bytes())
            .expect("ed25519 encoding key"),
        jwk: serde_json::json!({
            "kty": "OKP",
            "kid": kid,
            "use": "sig",
            "crv": "Ed25519",
            "x": URL_SAFE_NO_PAD.encode(pair.public_key_raw()),
        }),
    }
}

/// One key of each kind, made once for the whole binary, each under its own kid.
struct Keys {
    rsa: SigningKey,
    p256: SigningKey,
    p384: SigningKey,
    ed25519: SigningKey,
}

fn keys() -> &'static Keys {
    static KEYS: OnceLock<Keys> = OnceLock::new();
    KEYS.get_or_init(|| Keys {
        rsa: rsa_key("rsa"),
        p256: p256_key("p256"),
        p384: p384_key("p384"),
        ed25519: ed25519_key("ed25519"),
    })
}

/// The key that signs `alg`, and its kid.
fn key_for(alg: Algorithm) -> (&'static SigningKey, &'static str) {
    let k = keys();
    match alg {
        Algorithm::RS256
        | Algorithm::RS384
        | Algorithm::RS512
        | Algorithm::PS256
        | Algorithm::PS384
        | Algorithm::PS512 => (&k.rsa, "rsa"),
        Algorithm::ES256 => (&k.p256, "p256"),
        Algorithm::ES384 => (&k.p384, "p384"),
        Algorithm::EdDSA => (&k.ed25519, "ed25519"),
        other => panic!("no test key signs {other:?}"),
    }
}

/// A JWKS with every test key in it.
fn all_jwks() -> Vec<u8> {
    let k = keys();
    jwks_doc(&[
        k.rsa.jwk.clone(),
        k.p256.jwk.clone(),
        k.p384.jwk.clone(),
        k.ed25519.jwk.clone(),
    ])
}

// ---------------------------------------------------------------------------
// Tokens and validators
// ---------------------------------------------------------------------------

/// The claims a projected Kubernetes service account token carries, expiring `exp_in` seconds from now.
fn claims(exp_in: i64) -> serde_json::Value {
    serde_json::json!({
        "iss": TEST_ISSUER,
        "aud": TEST_AUDIENCE,
        "sub": SUBJECT,
        "exp": now_plus(exp_in),
        "kubernetes.io": {
            "namespace": "polar",
            "pod": { "name": "git-observer-abc", "uid": POD_UID },
            "serviceaccount": { "name": "git-observer", "uid": "99999999-8888-7777-6666-555555555555" },
        }
    })
}

/// A token whose header says `alg` and `kid`, signed with `key` as `alg`.
fn mint(alg: Algorithm, kid: &str, key: &EncodingKey, claims: &serde_json::Value) -> String {
    let mut header = Header::new(alg);
    header.kid = Some(kid.to_string());
    encode(&header, claims, key).unwrap_or_else(|e| panic!("encode {alg:?}: {e}"))
}

/// A token signed as `alg` with its own key, under its own kid.
fn token(alg: Algorithm, claims: &serde_json::Value) -> String {
    let (key, kid) = key_for(alg);
    mint(alg, kid, &key.encoding, claims)
}

/// A token with a hand-written header (any `alg` string at all) over `claims`, with `signature` as its third part.
fn raw_token(header: serde_json::Value, claims: &serde_json::Value, signature: &str) -> String {
    let h = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let c = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    format!("{h}.{c}.{signature}")
}

fn config(allowed: &[&str]) -> IssuerConfig {
    IssuerConfig {
        issuer: TEST_ISSUER.to_string(),
        audience: vec![TEST_AUDIENCE.to_string()],
        jwks_uri: Some("https://test-issuer.example.com/jwks".to_string()),
        workload_identity_claim: "sub".to_string(),
        instance_binding_claim: "kubernetes.io/pod/uid".to_string(),
        identity_normalization: IdentityNormalization::KubernetesServiceAccount {
            cluster_domain: "cluster.local".to_string(),
        },
        identity_scope_claim: None,
        allowed_algorithms: allowed.iter().map(|s| s.to_string()).collect(),
        jwks_cache_ttl_min: Duration::from_secs(30),
        jwks_cache_ttl_max: Duration::from_secs(3600),
    }
}

fn validator_for(cfg: IssuerConfig, jwks: Vec<u8>) -> (Validator, Arc<FakeJwksSource>) {
    let source = Arc::new(FakeJwksSource::new(jwks));
    (
        Validator::with_jwks_source(vec![cfg], source.clone()),
        source,
    )
}

/// A validator over every test key that allows `allowed`.
fn validator(allowed: &[&str]) -> (Validator, Arc<FakeJwksSource>) {
    validator_for(config(allowed), all_jwks())
}

fn every_mapped_name() -> Vec<&'static str> {
    MAPPED.iter().map(|(name, _)| *name).collect()
}

// ---------------------------------------------------------------------------
// Accepted
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_mapped_algorithm_verifies_when_its_issuer_allows_it() {
    let mut wrong = Vec::new();
    for (name, alg) in MAPPED {
        let (v, _) = validator(&[name]);
        match v.validate(&token(alg, &claims(300))).await {
            Ok(c) => {
                assert_eq!(c.workload_identity, SUBJECT, "{name}");
                assert_eq!(c.instance_binding.as_deref(), Some(POD_UID), "{name}");
                assert_eq!(c.issuer, TEST_ISSUER, "{name}");
                assert_eq!(c.audience, TEST_AUDIENCE, "{name}");
            }
            Err(e) => wrong.push(format!("{name}: {e:?}")),
        }
    }
    assert!(wrong.is_empty(), "refused: {wrong:?}");
}

#[tokio::test]
async fn the_deployed_list_accepts_rs256_es256_and_eddsa_and_nothing_else() {
    let (v, _) = validator(&DEPLOYED);
    for (name, alg) in MAPPED {
        let got = v.validate(&token(alg, &claims(300))).await;
        if DEPLOYED.contains(&name) {
            assert!(got.is_ok(), "{name}: {got:?}");
        } else {
            assert_eq!(
                got,
                Err(ValidationError::ForbiddenAlgorithm(name.to_string())),
                "{name}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Refused by the allow-list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn each_algorithm_is_forbidden_when_its_issuer_allows_every_other_one() {
    for (name, alg) in MAPPED {
        let others: Vec<&str> = every_mapped_name()
            .into_iter()
            .filter(|n| *n != name)
            .collect();
        let (v, source) = validator(&others);
        assert_eq!(
            v.validate(&token(alg, &claims(300))).await,
            Err(ValidationError::ForbiddenAlgorithm(name.to_string())),
            "{name}"
        );
        assert_eq!(source.fetch_count(), 0, "{name}: refused before any fetch");
    }
}

#[tokio::test]
async fn a_configured_name_the_code_does_not_map_allows_nothing() {
    // parse_allowed_algorithms matches the names exactly; anything else is dropped, so a list of only such names
    // allows no algorithm at all.
    for listed in [
        "rs256", "Rs256", "ES512", "ES256K", "Ed25519", "RSA", "PS1024",
    ] {
        let (v, source) = validator(&[listed]);
        for (name, alg) in MAPPED {
            assert_eq!(
                v.validate(&token(alg, &claims(300))).await,
                Err(ValidationError::ForbiddenAlgorithm(name.to_string())),
                "configured {listed}, token {name}"
            );
        }
        assert_eq!(source.fetch_count(), 0, "configured {listed}");
    }
}

// ---------------------------------------------------------------------------
// Refused whatever the configuration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn alg_none_in_any_case_is_forbidden_before_any_fetch() {
    let (v, source) = validator(&every_mapped_name());
    let body = claims(300);
    // A real ES256 signature over a different header, to show the third part is never looked at.
    let foreign = token(Algorithm::ES256, &body);
    let foreign_sig = foreign.rsplit('.').next().unwrap().to_string();
    for alg in ["none", "None", "NONE", "nOnE"] {
        for sig in ["", foreign_sig.as_str()] {
            let t = raw_token(
                serde_json::json!({ "alg": alg, "typ": "JWT", "kid": "p256" }),
                &body,
                sig,
            );
            assert_eq!(
                v.validate(&t).await,
                Err(ValidationError::ForbiddenAlgorithm(alg.to_string())),
                "{alg}, signature {}",
                if sig.is_empty() { "empty" } else { "present" }
            );
        }
        // And with no third part at all.
        let two_parts = raw_token(serde_json::json!({ "alg": alg }), &body, "");
        let two_parts = two_parts.trim_end_matches('.');
        assert_eq!(
            v.validate(two_parts).await,
            Err(ValidationError::ForbiddenAlgorithm(alg.to_string())),
            "{alg}, two parts"
        );
    }
    assert_eq!(source.fetch_count(), 0);
}

#[tokio::test]
async fn hmac_is_forbidden_even_when_configured_and_keyed_with_the_issuers_public_key() {
    // The classic confusion: an HS* token whose secret is the issuer's public key material. The configuration lists
    // the HS names too (the config validator would refuse that; the validator drops them, and refuses HS* outright).
    let mut allowed = every_mapped_name();
    allowed.extend(["HS256", "HS384", "HS512"]);
    let (v, source) = validator(&allowed);
    let public_n = keys().rsa.jwk["n"].as_str().unwrap().as_bytes().to_vec();
    for alg in [Algorithm::HS256, Algorithm::HS384, Algorithm::HS512] {
        let t = mint(
            alg,
            "rsa",
            &EncodingKey::from_secret(&public_n),
            &claims(300),
        );
        assert_eq!(
            v.validate(&t).await,
            Err(ValidationError::ForbiddenAlgorithm(format!("{alg:?}"))),
            "{alg:?}"
        );
    }
    assert_eq!(source.fetch_count(), 0);
}

#[tokio::test]
async fn an_alg_jsonwebtoken_cannot_represent_is_an_invalid_token_before_any_fetch() {
    // The pre-flight names only `none` and HS* (case-sensitive), so anything else jsonwebtoken has no variant for
    // fails the header parse: InvalidToken, not ForbiddenAlgorithm.
    let (v, source) = validator(&every_mapped_name());
    let body = claims(300);
    let es256 = token(Algorithm::ES256, &body);
    let sig = es256.rsplit('.').next().unwrap();
    for alg in [
        "ES512", "ES256K", "Ed25519", "Ed448", "RS1", "hs256", "Hs256", "rs256", "", "RSA-OAEP",
    ] {
        let t = raw_token(serde_json::json!({ "alg": alg, "kid": "p256" }), &body, sig);
        assert_eq!(
            v.validate(&t).await,
            Err(ValidationError::InvalidToken),
            "alg {alg:?}"
        );
    }
    // A header with no `alg` at all.
    let t = raw_token(serde_json::json!({ "kid": "p256" }), &body, sig);
    assert_eq!(v.validate(&t).await, Err(ValidationError::InvalidToken));
    assert_eq!(source.fetch_count(), 0);
}

#[tokio::test]
async fn a_token_without_a_kid_is_an_invalid_token() {
    let (v, _) = validator(&every_mapped_name());
    for (name, alg) in MAPPED {
        let (key, _) = key_for(alg);
        let t = encode(&Header::new(alg), &claims(300), &key.encoding).unwrap();
        assert_eq!(
            v.validate(&t).await,
            Err(ValidationError::InvalidToken),
            "{name}"
        );
    }
}

// ---------------------------------------------------------------------------
// A key that does not match the token's algorithm
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_key_of_another_family_or_curve_is_an_invalid_token() {
    let (v, _) = validator(&every_mapped_name());
    let body = claims(300);
    // (the algorithm the token is signed with and names, the kid of a key of another family or curve)
    let cases = [
        (Algorithm::ES256, "rsa"),
        (Algorithm::ES256, "p384"),
        (Algorithm::ES256, "ed25519"),
        (Algorithm::ES384, "p256"),
        (Algorithm::ES384, "rsa"),
        (Algorithm::RS256, "p256"),
        (Algorithm::RS256, "ed25519"),
        (Algorithm::PS256, "p256"),
        (Algorithm::EdDSA, "p256"),
        (Algorithm::EdDSA, "rsa"),
    ];
    for (alg, other_kid) in cases {
        let (key, _) = key_for(alg);
        let t = mint(alg, other_kid, &key.encoding, &body);
        assert_eq!(
            v.validate(&t).await,
            Err(ValidationError::InvalidToken),
            "{alg:?} under kid {other_kid}"
        );
    }
}

#[tokio::test]
async fn a_token_relabelled_to_another_algorithm_of_the_same_key_is_an_invalid_token() {
    // The same RSA key verifies RS* and PS*; a signature made as one and presented as the other does not verify.
    let (v, _) = validator(&every_mapped_name());
    let body = claims(300);
    for (signed, named) in [
        (Algorithm::PS256, Algorithm::RS256),
        (Algorithm::RS256, Algorithm::PS256),
        (Algorithm::RS256, Algorithm::RS384),
    ] {
        let real = token(signed, &body);
        let sig = real.rsplit('.').next().unwrap();
        let t = raw_token(
            serde_json::json!({ "alg": format!("{named:?}"), "typ": "JWT", "kid": "rsa" }),
            &body,
            sig,
        );
        assert_eq!(
            v.validate(&t).await,
            Err(ValidationError::InvalidToken),
            "signed {signed:?}, named {named:?}"
        );
    }
}

#[tokio::test]
async fn a_jwk_that_is_symmetric_or_for_encryption_is_never_used() {
    let k = keys();
    let mut enc = k.p256.jwk.clone();
    enc["use"] = serde_json::json!("enc");
    let oct =
        serde_json::json!({ "kty": "oct", "kid": "rsa", "k": URL_SAFE_NO_PAD.encode(b"secret") });
    let (v, source) = validator_for(config(&every_mapped_name()), jwks_doc(&[enc, oct]));
    for alg in [Algorithm::ES256, Algorithm::RS256] {
        assert_eq!(
            v.validate(&token(alg, &claims(300))).await,
            Err(ValidationError::InvalidToken),
            "{alg:?}"
        );
    }
    assert!(
        source.fetch_count() >= 1,
        "the JWKS was read, and its keys skipped"
    );
}

// ---------------------------------------------------------------------------
// Expiry, audience, issuer: the same for every algorithm
// ---------------------------------------------------------------------------

#[tokio::test]
async fn expiry_is_required_and_checked_with_sixty_seconds_of_leeway() {
    let (v, _) = validator(&every_mapped_name());
    for (name, alg) in MAPPED {
        assert_eq!(
            v.validate(&token(alg, &claims(-300))).await,
            Err(ValidationError::Expired),
            "{name}: expired five minutes ago"
        );
        assert_eq!(
            v.validate(&token(alg, &claims(-90))).await,
            Err(ValidationError::Expired),
            "{name}: expired ninety seconds ago"
        );
        // jsonwebtoken's default leeway (Validation::new: 60s), which the validator does not change.
        assert!(
            v.validate(&token(alg, &claims(-30))).await.is_ok(),
            "{name}: expired thirty seconds ago is within the leeway"
        );
        let mut no_exp = claims(300);
        no_exp.as_object_mut().unwrap().remove("exp");
        assert_eq!(
            v.validate(&token(alg, &no_exp)).await,
            Err(ValidationError::MissingClaim("exp".to_string())),
            "{name}: no exp"
        );
    }
}

#[tokio::test]
async fn the_audience_must_be_given_and_be_one_configured() {
    let (v, _) = validator(&every_mapped_name());
    for (name, alg) in MAPPED {
        let mut wrong = claims(300);
        wrong["aud"] = serde_json::json!("some-other-audience");
        assert_eq!(
            v.validate(&token(alg, &wrong)).await,
            Err(ValidationError::InvalidAudience),
            "{name}: wrong audience"
        );
        wrong["aud"] = serde_json::json!(["some-other-audience", "and-another"]);
        assert_eq!(
            v.validate(&token(alg, &wrong)).await,
            Err(ValidationError::InvalidAudience),
            "{name}: a list without ours"
        );
        let mut listed = claims(300);
        listed["aud"] = serde_json::json!(["some-other-audience", TEST_AUDIENCE]);
        let got = v.validate(&token(alg, &listed)).await;
        assert_eq!(
            got.map(|c| c.audience),
            Ok(TEST_AUDIENCE.to_string()),
            "{name}: a list with ours reports ours"
        );
        let mut none = claims(300);
        none.as_object_mut().unwrap().remove("aud");
        assert_eq!(
            v.validate(&token(alg, &none)).await,
            Err(ValidationError::MissingClaim("aud".to_string())),
            "{name}: no aud"
        );
    }
}

#[tokio::test]
async fn an_untrusted_or_missing_issuer_is_refused_before_any_fetch() {
    let (v, source) = validator(&every_mapped_name());
    for (name, alg) in MAPPED {
        let mut other = claims(300);
        other["iss"] = serde_json::json!("https://someone-else.example.com");
        assert_eq!(
            v.validate(&token(alg, &other)).await,
            Err(ValidationError::InvalidIssuer(
                "https://someone-else.example.com".to_string()
            )),
            "{name}: untrusted issuer"
        );
        // An issuer that differs from the trusted one only in a trailing slash is another issuer.
        other["iss"] = serde_json::json!(format!("{TEST_ISSUER}/"));
        assert_eq!(
            v.validate(&token(alg, &other)).await,
            Err(ValidationError::InvalidIssuer(format!("{TEST_ISSUER}/"))),
            "{name}: trailing slash"
        );
        let mut none = claims(300);
        none.as_object_mut().unwrap().remove("iss");
        assert_eq!(
            v.validate(&token(alg, &none)).await,
            Err(ValidationError::InvalidToken),
            "{name}: no iss"
        );
    }
    assert_eq!(source.fetch_count(), 0);
}

#[tokio::test]
async fn a_signature_from_another_key_of_the_same_kind_is_an_invalid_token() {
    // The JWKS publishes one key under each kid; a token signed by a different key of the same kind, under that kid.
    let (v, _) = validator(&every_mapped_name());
    let body = claims(300);
    let impostors = [
        (Algorithm::ES256, "p256", p256_key("p256")),
        (Algorithm::ES384, "p384", p384_key("p384")),
        (Algorithm::EdDSA, "ed25519", ed25519_key("ed25519")),
    ];
    for (alg, kid, other) in impostors {
        assert_eq!(
            v.validate(&mint(alg, kid, &other.encoding, &body)).await,
            Err(ValidationError::InvalidToken),
            "{alg:?}"
        );
    }
    ensure_crypto_provider();
    let pem = secondary_keypair()
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .unwrap();
    let other_rsa = EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap();
    for alg in [
        Algorithm::PS256,
        Algorithm::PS384,
        Algorithm::PS512,
        Algorithm::RS512,
    ] {
        assert_eq!(
            v.validate(&mint(alg, "rsa", &other_rsa, &body)).await,
            Err(ValidationError::InvalidToken),
            "{alg:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Not before, issued at, and how long a token lives
// ---------------------------------------------------------------------------

/// The standard claims, with `changes` set (a null removes one).
fn claims_with(changes: serde_json::Value) -> serde_json::Value {
    let mut c = claims(300);
    for (k, v) in changes.as_object().unwrap() {
        if v.is_null() {
            c.as_object_mut().unwrap().remove(k);
        } else {
            c[k] = v.clone();
        }
    }
    c
}

#[tokio::test]
async fn a_token_not_valid_before_a_time_still_to_come_is_refused_beyond_the_leeway() {
    let (v, _) = validator(&every_mapped_name());
    for (name, alg) in MAPPED {
        for ahead in [120, 3600, 31_536_000] {
            assert_eq!(
                v.validate(&token(
                    alg,
                    &claims_with(serde_json::json!({ "nbf": now_plus(ahead) }))
                ))
                .await,
                Err(ValidationError::NotYetValid("nbf".to_string())),
                "{name}: nbf {ahead}s ahead"
            );
        }
        // Within the 60 seconds of leeway, and in the past: accepted.
        for at in [30, 0, -600] {
            let got = v
                .validate(&token(
                    alg,
                    &claims_with(serde_json::json!({ "nbf": now_plus(at) })),
                ))
                .await;
            assert!(got.is_ok(), "{name}: nbf {at}s from now: {got:?}");
        }
    }
}

#[tokio::test]
async fn a_token_issued_in_the_future_is_refused_beyond_the_leeway() {
    let (v, _) = validator(&every_mapped_name());
    for (name, alg) in MAPPED {
        for ahead in [120, 3600, 31_536_000] {
            assert_eq!(
                v.validate(&token(
                    alg,
                    &claims_with(serde_json::json!({ "iat": now_plus(ahead) }))
                ))
                .await,
                Err(ValidationError::NotYetValid("iat".to_string())),
                "{name}: iat {ahead}s ahead"
            );
        }
        for at in [30, 0, -60] {
            let got = v
                .validate(&token(
                    alg,
                    &claims_with(serde_json::json!({ "iat": now_plus(at) })),
                ))
                .await;
            assert!(got.is_ok(), "{name}: iat {at}s from now: {got:?}");
        }
    }
}

#[tokio::test]
async fn a_token_that_lives_longer_than_a_day_is_refused_and_one_that_lives_a_day_is_not() {
    let (v, _) = validator(&every_mapped_name());
    let day = DEFAULT_MAX_TOKEN_LIFETIME.as_secs() as i64;
    assert_eq!(day, 86_400);
    for (name, alg) in MAPPED {
        // exp - iat, exactly at the maximum and one second past it.
        let at_most = claims_with(
            serde_json::json!({ "iat": now_plus(0) - 10, "exp": now_plus(0) - 10 + day }),
        );
        let got = v.validate(&token(alg, &at_most)).await;
        assert!(got.is_ok(), "{name}: a day exactly: {got:?}");
        let over = claims_with(
            serde_json::json!({ "iat": now_plus(0) - 10, "exp": now_plus(0) - 10 + day + 1 }),
        );
        assert_eq!(
            v.validate(&token(alg, &over)).await,
            Err(ValidationError::LifetimeTooLong {
                lifetime_secs: 86_401,
                max_secs: 86_400
            }),
            "{name}: a day and a second"
        );
        // Issued long ago and near its end: what it was issued for is what counts.
        let old =
            claims_with(serde_json::json!({ "iat": now_plus(-80_000), "exp": now_plus(10_000) }));
        assert_eq!(
            v.validate(&token(alg, &old)).await,
            Err(ValidationError::LifetimeTooLong {
                lifetime_secs: 90_000,
                max_secs: 86_400
            }),
            "{name}: issued for 25 hours"
        );
        // A century out, as the probe found accepted before.
        let century =
            claims_with(serde_json::json!({ "iat": now_plus(0), "exp": now_plus(3_153_600_000) }));
        assert!(
            matches!(
                v.validate(&token(alg, &century)).await,
                Err(ValidationError::LifetimeTooLong { .. })
            ),
            "{name}: a century"
        );
        // Without iat, what is left of it is bounded instead: exp - now.
        let no_iat_far =
            claims_with(serde_json::json!({ "iat": null, "exp": now_plus(day + 120) }));
        assert!(
            matches!(
                v.validate(&token(alg, &no_iat_far)).await,
                Err(ValidationError::LifetimeTooLong { .. })
            ),
            "{name}: no iat, a day and two minutes left"
        );
        let no_iat_near =
            claims_with(serde_json::json!({ "iat": null, "exp": now_plus(day - 120) }));
        let got = v.validate(&token(alg, &no_iat_near)).await;
        assert!(got.is_ok(), "{name}: no iat, under a day left: {got:?}");
    }
}

#[tokio::test]
async fn the_maximum_lifetime_given_to_the_validator_is_the_one_it_holds_to() {
    let (v, _) = validator(&every_mapped_name());
    let v = v.with_max_token_lifetime(Duration::from_secs(3600));
    let hour = claims_with(serde_json::json!({ "iat": now_plus(0), "exp": now_plus(3600) }));
    let longer = claims_with(serde_json::json!({ "iat": now_plus(0), "exp": now_plus(3601) }));
    for (name, alg) in MAPPED {
        let got = v.validate(&token(alg, &hour)).await;
        assert!(got.is_ok(), "{name}: an hour: {got:?}");
        assert_eq!(
            v.validate(&token(alg, &longer)).await,
            Err(ValidationError::LifetimeTooLong {
                lifetime_secs: 3601,
                max_secs: 3600
            }),
            "{name}: an hour and a second"
        );
    }
}

#[test]
fn the_refusals_say_what_is_wrong() {
    assert_eq!(
        ValidationError::NotYetValid("nbf".to_string()).to_string(),
        "token is not yet valid: its 'nbf' is in the future"
    );
    assert_eq!(
        ValidationError::LifetimeTooLong {
            lifetime_secs: 90_000,
            max_secs: 86_400
        }
        .to_string(),
        "token lives 90000s, beyond the 86400s allowed"
    );
}

// ---------------------------------------------------------------------------
// The `kubernetes.io` claims
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_kubernetes_io_claims_are_read_by_slash_paths_for_every_algorithm() {
    for (name, alg) in MAPPED {
        // The service account's name, from the nested claim, as the workload identity; the pod's uid as the binding.
        let mut cfg = config(&[name]);
        cfg.workload_identity_claim = "/kubernetes.io/serviceaccount/name".to_string();
        let (v, _) = validator_for(cfg, all_jwks());
        let c = v
            .validate(&token(alg, &claims(300)))
            .await
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(c.workload_identity, "git-observer", "{name}");
        assert_eq!(c.instance_binding.as_deref(), Some(POD_UID), "{name}");
        assert_eq!(c.raw["kubernetes.io"]["namespace"], "polar", "{name}");

        // A dotted path is not a path: the key "kubernetes.io" is one segment. The binding is optional, so the token
        // is still accepted, without one.
        let mut dotted = config(&[name]);
        dotted.instance_binding_claim = "kubernetes.io.pod.uid".to_string();
        let (v, _) = validator_for(dotted, all_jwks());
        let c = v.validate(&token(alg, &claims(300))).await.unwrap();
        assert_eq!(c.instance_binding, None, "{name}: dotted path");

        // A workload identity claim that is absent, or not a string, refuses the token.
        let mut missing = config(&[name]);
        missing.workload_identity_claim = "kubernetes.io/serviceaccount/missing".to_string();
        let (v, _) = validator_for(missing, all_jwks());
        assert_eq!(
            v.validate(&token(alg, &claims(300))).await,
            Err(ValidationError::MissingClaim(
                "kubernetes.io/serviceaccount/missing".to_string()
            )),
            "{name}: absent"
        );
        let mut object = config(&[name]);
        object.workload_identity_claim = "kubernetes.io/pod".to_string();
        let (v, _) = validator_for(object, all_jwks());
        assert_eq!(
            v.validate(&token(alg, &claims(300))).await,
            Err(ValidationError::MissingClaim(
                "kubernetes.io/pod".to_string()
            )),
            "{name}: not a string"
        );
    }
}
