let values = ../values.dhall

let GitRepository = { apiVersion = "source.toolkit.fluxcd.io/v1"
, kind = "GitRepository"
, metadata = { name = values.deployRepository.name, namespace = values.namespace }
, spec = values.deployRepository.spec
}

in GitRepository