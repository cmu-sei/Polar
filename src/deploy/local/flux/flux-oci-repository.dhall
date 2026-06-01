-- flux-oci-repository.dhall
--
-- Flux OCIRepository pointing at the local insecure registry.
-- Tells source-controller to poll for new digests on the given tag.
--
-- Generate and apply:
--   dhall-to-yaml <<< ./flux-oci-repository.dhall | kubectl apply -f -
--
-- Watch:
--   kubectl get ocirepository polar-test -n flux-system -w
--
-- Prerequisites:
--   Push an artifact first:
--     flux push artifact oci://localhost:31500/polar-test:latest \
--       --path=./test/manifests \
--       --source=https://github.com/cmu-sei/Polar \
--       --revision=main@sha1:$(git rev-parse HEAD)
--
--   The --revision flag sets org.opencontainers.image.revision on the artifact,
--   which Flux surfaces as status.lastAppliedOriginRevision on the Kustomization.
--   Without it that field stays null and the full chain cannot be verified.

{ apiVersion = "source.toolkit.fluxcd.io/v1"
, kind = "OCIRepository"
, metadata =
  { name = "polar-test"
  , namespace = "flux-system"
  }
, spec =
  { interval = "1m"
    -- In-cluster address of the local registry (ClusterIP service).
    -- Use localhost:31500 only from the host when pushing.
  , url = "oci://registry.local-registry.svc.cluster.local:5000/manifests/polar"
  , ref = { tag = "latest" }
    -- Required because the local registry has no TLS.
  , insecure = True
  }
}
