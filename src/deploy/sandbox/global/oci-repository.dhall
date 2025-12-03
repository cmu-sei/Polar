let Constants = ../../types/constants.dhall
let values = ../values.dhall
in { apiVersion = "source.toolkit.fluxcd.io/v1"
, kind = "OCIRepository"
, metadata = { name = values.deployRepository.name, namespace = Constants.PolarNamespace }
, spec =
  { interval = "10m"
  , ref.tag = "sandbox"
  , url = values.sandboxRegistry.url
  , secretRef = { name = "flux-repo-secret" }
  }
}
