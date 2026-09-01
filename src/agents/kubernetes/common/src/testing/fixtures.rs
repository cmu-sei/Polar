//! Loads synthetic Kubernetes resources from YAML fixtures for Tier 1
//! harness scenarios (issue #221) -- constructing typed `k8s_openapi`/
//! `kube_common` resources directly, bypassing the observer, the watch
//! loop, and Cassini entirely, so a scenario is "here is a Pod" rather
//! than "here is a cluster that produces a Pod event."
//!
//! Fixtures are ordinary Kubernetes manifests -- the same YAML you'd get
//! from `kubectl get pod foo -o yaml` -- not a bespoke test format. That's
//! deliberate: it means a fixture can be captured directly from a real
//! cluster (or a real incident) with no translation step, and it means
//! this loader has nothing project-specific to drift out of sync with as
//! the schema evolves. The type parameter is the only thing that changes
//! per resource kind; the loading logic is identical for all of them,
//! which is the whole point of scenarios being "typed resources," not
//! "typed resources plus a parser per kind."

use serde::de::DeserializeOwned;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("failed to read fixture file {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse fixture as {type_name}: {source}")]
    Parse {
        type_name: &'static str,
        #[source]
        source: serde_yaml::Error,
    },
}

/// Parses a resource directly from a YAML string already in memory.
///
/// `T` is any type implementing `Deserialize` -- in practice every
/// `k8s_openapi`/`kube_common` resource type already does, since they're
/// generated against (or modeled on) the real Kubernetes wire format.
/// `kubectl get -o yaml` output, top-level `apiVersion`/`kind` included,
/// deserializes directly: those two keys aren't struct fields on the
/// concrete resource types (they're `Resource::API_VERSION`/`KIND`
/// associated constants instead), and k8s_openapi types don't set
/// `#[serde(deny_unknown_fields)]` -- forward-compatibility with fields
/// the client doesn't know about requires tolerating them. I'm confident
/// in this for the resource types used throughout this codebase (Pod,
/// Deployment, ReplicaSet, Job, Namespace, Node, and the Flux CRDs), but
/// haven't verified it against every field shape k8s_openapi generates,
/// so treat the first real fixture you load as the actual confirmation,
/// not this comment.
pub fn parse_fixture<T: DeserializeOwned>(yaml: &str) -> Result<T, FixtureError> {
    serde_yaml::from_str(yaml).map_err(|source| FixtureError::Parse {
        type_name: std::any::type_name::<T>(),
        source,
    })
}

/// Loads and parses a resource from a fixture file on disk.
pub fn load_fixture<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T, FixtureError> {
    let path_ref = path.as_ref();
    let yaml = std::fs::read_to_string(path_ref).map_err(|source| FixtureError::Io {
        path: path_ref.display().to_string(),
        source,
    })?;
    parse_fixture(&yaml)
}

/// Loads every `*.yaml`/`*.yml` file in a directory as `T`, sorted by
/// filename. Sorted specifically so a scenario's event ordering is
/// determined by filename (`01-pod-pending.yaml`, `02-pod-running.yaml`,
/// ...) rather than directory-read order, which is not guaranteed stable
/// across platforms and would make a "scenario" silently non-deterministic
/// in exactly the way issue #221 is trying to eliminate.
pub fn load_fixture_dir<T: DeserializeOwned>(
    dir: impl AsRef<Path>,
) -> Result<Vec<(String, T)>, FixtureError> {
    let dir_ref = dir.as_ref();
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir_ref)
        .map_err(|source| FixtureError::Io {
            path: dir_ref.display().to_string(),
            source,
        })?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("yaml") | Some("yml")
            )
        })
        .collect();
    entries.sort();

    entries
        .into_iter()
        .map(|path| {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let resource = load_fixture::<T>(&path)?;
            Ok((name, resource))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::Pod;

    // Deliberately written the way a real fixture would be authored --
    // copy-pasted kubectl output, apiVersion/kind included, not hand-
    // trimmed to whatever's minimally necessary to compile. If tolerating
    // apiVersion/kind is wrong, this is where it fails, not in a comment.
    const POD_FIXTURE: &str = r#"
apiVersion: v1
kind: Pod
metadata:
  name: test-pod
  namespace: default
  uid: 11111111-1111-1111-1111-111111111111
spec:
  nodeName: test-node-1
  containers:
    - name: main
      image: example.com/app:1.0.0
status:
  phase: Running
  containerStatuses:
    - name: main
      ready: true
      imageID: example.com/app@sha256:deadbeef
      image: example.com/app:1.0.0
      state:
        running:
          startedAt: "2026-08-31T00:00:00Z"
"#;

    #[test]
    fn parses_a_realistic_pod_manifest_including_apiversion_and_kind() {
        let pod: Pod = parse_fixture(POD_FIXTURE).expect("realistic Pod fixture must parse");

        assert_eq!(pod.metadata.name.as_deref(), Some("test-pod"));
        assert_eq!(pod.metadata.namespace.as_deref(), Some("default"));
        assert_eq!(
            pod.spec.as_ref().and_then(|s| s.node_name.as_deref()),
            Some("test-node-1")
        );
        assert_eq!(
            pod.status.as_ref().and_then(|s| s.phase.as_deref()),
            Some("Running")
        );
    }

    #[test]
    fn reports_the_target_type_on_parse_failure() {
        let result: Result<Pod, _> = parse_fixture("not: [valid, pod, shape");
        let err = result.expect_err("malformed YAML must fail, not silently default");
        assert!(matches!(err, FixtureError::Parse { .. }));
    }
}
