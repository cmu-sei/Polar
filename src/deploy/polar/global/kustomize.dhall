let values = ../values.dhall

let kustomize = { apiVersion = "kustomize.toolkit.fluxcd.io/v1"
, kind = "Kustomization"
, metadata = { name = "polar", namespace = values.namespace }
, spec =
  { decryption = { provider = "sops" } 
  , interval = "5m"
  , path = "./manifests"
  , prune = True
  , sourceRef = { kind = "GitRepository", name = values.deployRepository.name }
  }
}

in kustomize