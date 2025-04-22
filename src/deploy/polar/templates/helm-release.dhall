let values = ../values.dhall

let release = { apiVersion = "helm.toolkit.fluxcd.io/v2"
, kind = "HelmRelease"
, metadata = { name = "polar", namespace = values.namespace }
, spec =
  { chart.spec
    =
    { chart = "polar"
    , sourceRef =
      { kind = "GitRepository"
      , name = values.deployRepository.name
      , namespace = values.namespace
      }
    }
  , interval = "5m"
  }
}

in release