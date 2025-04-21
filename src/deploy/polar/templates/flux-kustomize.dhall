let values = ../values.dhall

let Kustomization = { apiVersion = "kustomize.toolkit.fluxcd.io/v1"
, kind = "Kustomization"
, metadata = { name = "polar", namespace = values.namespace }
, spec =
  { interval = "10m"
  , path = "./chart"
  , prune = True
  , sourceRef = { kind = "GitRepository", name = values.deployRepository.name }
  , targetNamespace = values.namespace
  , timeout = "1m"
  }
}
in Kustomization