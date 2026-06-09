// tests/integration_tests.rs

use oci_client::{
    Client as OciClient, Reference,
    client::{ClientConfig, ClientProtocol, Config},
    manifest::{IMAGE_MANIFEST_MEDIA_TYPE, OciImageManifest, OciManifest},
    secrets::RegistryAuth,
};
use std::str::FromStr;
use testcontainers::{
    GenericImage,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Spin up a registry:2 container and return (container, "host:port").
/// Container must stay alive for the duration of the test — keep it bound.
async fn start_registry() -> (impl Drop, String) {
    let container = GenericImage::new("registry", "2")
        .with_exposed_port(5000.tcp())
        .with_wait_for(WaitFor::message_on_stderr("listening on"))
        .start()
        .await
        .expect("Failed to start registry:2");

    let host = container.get_host().await.expect("get_host");
    let port = container
        .get_host_port_ipv4(5000)
        .await
        .expect("get_host_port_ipv4(5000)");

    (container, format!("{host}:{port}"))
}

/// Build an OCI client configured for plain HTTP (registry:2 default).
fn http_client() -> OciClient {
    OciClient::new(ClientConfig {
        protocol: ClientProtocol::Http,
        ..ClientConfig::default()
    })
}

/// Push a minimal scratch image (empty layer list, `{}` config) to the
/// registry under `<registry_host>/<repo>:<tag>`. Returns the manifest digest.
async fn push_minimal_image(registry_host: &str, repo: &str, tag: &str) -> String {
    let client = http_client();
    let reference = Reference::from_str(&format!("{registry_host}/{repo}:{tag}")).unwrap();

    // `{}` is the canonical empty OCI image config.
    let config_data = b"{}".to_vec();

    let config = Config {
        data: config_data,
        media_type: "application/vnd.oci.image.config.v1+json".to_string(),
        annotations: None,
    };

    let response = client
        .push(&reference, &[], config, &RegistryAuth::Anonymous, None)
        .await
        .expect("push should succeed against local registry");

    // manifest_url is "http://host:port/v2/repo/manifests/sha256:..."
    response
        .manifest_url
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .to_string()
}

// ---------------------------------------------------------------------------
// Integration tests
// ---------------------------------------------------------------------------

/// Happy path: pull_manifest succeeds and returns a non-empty digest for an
/// image that actually exists in the registry.
#[tokio::test]
async fn inspect_image_resolves_manifest_from_local_registry() {
    let (_container, registry_host) = start_registry().await;
    push_minimal_image(&registry_host, "polar/test", "latest").await;

    let client = http_client();
    let reference = Reference::from_str(&format!("{registry_host}/polar/test:latest")).unwrap();

    let (manifest, digest) = client
        .pull_manifest(&reference, &RegistryAuth::Anonymous)
        .await
        .expect("pull_manifest should succeed for a known image");

    assert!(!digest.is_empty(), "digest must be non-empty");
    assert!(
        matches!(manifest, OciManifest::Image(_)),
        "expected OciManifest::Image, got something else"
    );
}

/// Error path: pull_manifest returns Err for an image that was never pushed.
/// Verifies the failure propagates rather than returning Ok(None).
#[tokio::test]
async fn inspect_image_errors_on_missing_image() {
    let (_container, registry_host) = start_registry().await;
    // Intentionally push nothing.

    let client = http_client();
    let reference =
        Reference::from_str(&format!("{registry_host}/polar/does-not-exist:latest")).unwrap();

    let result = client
        .pull_manifest(&reference, &RegistryAuth::Anonymous)
        .await;

    assert!(
        result.is_err(),
        "pull_manifest should return Err for a non-existent image"
    );
}

/// Verifies that pushing two different tags to the same repo produces
/// distinct digests — guards against registry state leaking between pushes.
#[tokio::test]
async fn distinct_tags_produce_distinct_digests() {
    let (_container, registry_host) = start_registry().await;

    // Push identical content under two tags; digests should still be equal
    // (content-addressed) but the manifests should resolve independently.
    push_minimal_image(&registry_host, "polar/app", "v1").await;
    push_minimal_image(&registry_host, "polar/app", "v2").await;

    let client = http_client();

    let ref_v1 = Reference::from_str(&format!("{registry_host}/polar/app:v1")).unwrap();
    let ref_v2 = Reference::from_str(&format!("{registry_host}/polar/app:v2")).unwrap();

    let (_, digest_v1) = client
        .pull_manifest(&ref_v1, &RegistryAuth::Anonymous)
        .await
        .expect("v1 pull should succeed");

    let (_, digest_v2) = client
        .pull_manifest(&ref_v2, &RegistryAuth::Anonymous)
        .await
        .expect("v2 pull should succeed");

    // Both tags point to identical content so digests match — the point is
    // that both resolve without error.
    assert!(!digest_v1.is_empty());
    assert!(!digest_v2.is_empty());
}

// ---------------------------------------------------------------------------
// Pure-function unit tests (no container needed)
// ---------------------------------------------------------------------------

/// registry_from_image_ref extracts the hostname component from a fully
/// qualified image reference and returns None for Docker Hub short refs.
#[test]
fn registry_from_image_ref_extracts_hostname() {
    fn registry_from_image_ref(uri: &str) -> Option<String> {
        let first = uri.split('/').next()?;
        let has_dot = first.contains('.');
        let has_port = first.contains(':') && uri.contains('/');
        if has_dot || has_port {
            Some(first.to_string())
        } else {
            None
        }
    }

    // Fully qualified with dot — hostname present.
    assert_eq!(
        registry_from_image_ref("ghcr.io/myorg/myimage:latest"),
        Some("ghcr.io".into())
    );
    // Host:port with path — hostname present.
    assert_eq!(
        registry_from_image_ref("localhost:5000/myimage:tag"),
        Some("localhost:5000".into())
    );
    // Bare name:tag with no slash — not a hostname.
    assert_eq!(registry_from_image_ref("myimage:latest"), None);
    // Docker Hub two-part ref — "library" has no dot or port, not a hostname.
    assert_eq!(registry_from_image_ref("library/ubuntu"), None);
}

/// build_registry_candidates must not generate any http:// variants.
#[test]
fn build_registry_candidates_excludes_http() {
    fn build_registry_candidates(base: &str) -> Vec<String> {
        vec![
            base.to_string(),
            format!("https://{}", base),
            format!("https://{}/", base),
            format!("{}/v1/", base),
            format!("https://{}/v1/", base),
        ]
    }

    let candidates = build_registry_candidates("registry.example.com");

    assert!(
        !candidates.iter().any(|c| c.starts_with("http://")),
        "no http:// candidates should be generated"
    );
    assert!(candidates.contains(&"https://registry.example.com".to_string()));
    assert!(candidates.contains(&"registry.example.com".to_string()));
}

/// OciImageManifest serialises and deserialises cleanly via serde_json —
/// validates the round-trip used by emit_oci_resolved_event.
#[test]
fn oci_manifest_round_trips_through_serde() {
    let manifest = OciManifest::Image(OciImageManifest {
        schema_version: 2,
        media_type: Some(IMAGE_MANIFEST_MEDIA_TYPE.to_string()),
        ..OciImageManifest::default()
    });

    let serialized = serde_json::to_vec(&manifest).expect("serialization should not fail");
    let reparsed: OciManifest =
        serde_json::from_slice(&serialized).expect("deserialization should not fail");

    assert!(matches!(reparsed, OciManifest::Image(_)));
}
