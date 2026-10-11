-- infra/layers/3-workloads/agents/scheduler/values.dhall
--
-- Canonical defaults for the polar-scheduler chart.
-- (The observer is a catalog workload: infra/config/workloads/scheduler.nuon.)
-- Processor: reads scheduling events from Cassini and writes to graph.
--
-- The schedules repo credentials are injected at render time from secrets.
-- POLAR_SCHEDULER_REMOTE_URL and related vars are threaded in via context.

let Constants = ../../../../schema/constants.dhall

in  { name            = "polar-scheduler"
    , imagePullPolicy = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , processor =
      { name  = "polar-scheduler"
      , image = "polar-scheduler-processor:latest"
      }

    , proxyCACert = None Text
    }
