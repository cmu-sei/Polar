let Constants = ../../types/constants.dhall
let values = ../values.dhall
-- creates an OCI Repository to enable some gitops trough flux.
-- ---- IMPORTANT!!!! ---
-- Did you create the secret?
--
in { apiVersion = "source.toolkit.fluxcd.io/v1"
, kind = "OCIRepository"
, metadata = { name = values.deployRepository.name, namespace = Constants.PolarNamespace }
, spec =
  { interval = "10m"
  , ref.tag = "sandbox"
  , url = "oci://${values.sandboxRegistry.url}/polar/polar-manifests"
  , secretRef = { name = "flux-repo-secret" }
  }
}
