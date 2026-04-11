-- infra/layers/3-workloads/agents/scheduler/values.dhall
--
-- Canonical defaults for the polar-scheduler chart.
-- Observer: syncs a GitOps schedules repo and publishes scheduling events.
-- Processor: reads scheduling events from Cassini and writes to graph.
--
-- The schedules repo credentials are injected at render time from secrets.
-- POLAR_SCHEDULER_REMOTE_URL and related vars are threaded in via context.

let Constants = ../../../../schema/constants.dhall

in  { name            = "polar-scheduler"
    , imagePullPolicy = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , observer =
      { name         = "polar-scheduler-observer"
      , image        = "polar-scheduler-observer:latest"
      , syncInterval = "120"
      , localPath    = "/tmp/polar-schedules"
      }

    , processor =
      { name  = "polar-scheduler"
      , image = "polar-scheduler-processor:latest"
      }

    , tls =
      { certificateRequestName = "scheduler-agent-certificate"
      , certificateSpec =
        { commonName  = Constants.mtls.commonName
        , dnsNames    = [ Constants.cassiniDNSName ]
        , duration    = "2160h"
        , issuerRef   = { kind = "Issuer", name = Constants.mtls.leafIssuerName }
        , renewBefore = "360h"
        , secretName  = "scheduler-agent-tls"
        }
      }

    , proxyCACert = None Text
    }
