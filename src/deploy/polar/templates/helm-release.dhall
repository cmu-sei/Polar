let values = ../values.dhall

let release = { apiVersion = "helm.toolkit.fluxcd.io/v2"
, kind = "HelmRelease"
, metadata = { name = "polar", namespace = values.namespace }
, spec =
  { chart.spec
    =
    { chart = "chart" -- Path to the chart, not the name
    , sourceRef =
      { kind = "GitRepository"
      , name = values.deployRepository.name
      , namespace = values.namespace
      }
      releaseName = "polar"
    }
  , interval = "5m"
  }
}

in release