-- flux-kustomization.dhall
--
-- Flux Kustomization that reconciles manifests from the OCIRepository above.
-- Tells kustomize-controller to apply whatever the source-controller resolved.
--
-- Generate and apply:
--   dhall-to-yaml <<< ./flux-kustomization.dhall | kubectl apply -f -
--
-- Watch:
--   kubectl get kustomization polar-test -n flux-system -w
--   flux logs --all-namespaces --follow
--
-- When reconciliation completes, verify the status fields that close the chain:
--   kubectl get kustomization polar-test -n flux-system -o jsonpath=\
--     '{.status.lastAppliedRevision}{"\n"}{.status.lastAppliedOriginRevision}{"\n"}'
--
-- lastAppliedRevision should be the OCI content digest (sha256:...).
-- lastAppliedOriginRevision should be the value you passed to --revision
-- when pushing the artifact.

{ apiVersion = "kustomize.toolkit.fluxcd.io/v1"
, kind = "Kustomization"
, metadata =
  { name = "polar-test"
  , namespace = "flux-system"
  }
, spec =
  { interval = "1m"
  , sourceRef =
    { kind = "OCIRepository"
      -- Must match metadata.name in flux-oci-repository.dhall exactly.
    , name = "polar-test"
    }
    -- "." means root of the artifact. Adjust if your kustomization.yaml
    -- lives in a subdirectory inside the artifact.
  , path = "."
  , prune = True
    -- Give the controller enough time to apply and health-check before
    -- marking the reconciliation as failed.
  , timeout = "2m"
  }
}
