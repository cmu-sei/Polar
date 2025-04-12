let values = ../values.dhall

let GitRepository = { apiVersion = "source.toolkit.fluxcd.io/v1"
, kind = "GitRepository"
, metadata = { name = values.deployRepo.name, namespace = values.namespace }
, spec = values.deployRepo.spec
}

in GitRepository