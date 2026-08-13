-- infra/layers/2-services/cassini/values.dhall
--
-- Canonical defaults for the cassini chart.
-- Targets override only what differs via targets/<target>/overrides.dhall.
--
-- jaegerDNSName is resolved by render.nu from jaeger's outputs and injected
-- at render time — it is not hardcoded here.

let Constants = ../../../schema/constants.dhall

in  { name            = "cassini"
    , image           = "cassini:latest"
    , imagePullPolicy = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }
    , ports           =
      { http = 3000
      , tcp  = 8080
      }
    }
