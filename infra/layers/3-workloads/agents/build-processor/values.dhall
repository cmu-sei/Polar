-- infra/layers/3-workloads/agents/build-processor/values.dhall
--
-- Canonical defaults for the build-processor agent chart.
-- Processes both build execution lifecycle events and artifact provenance
-- events in a single unified agent, writing to the graph. Consolidates
-- what used to be build-orchestrator's processor half and provenance's
-- linker half into one binary.

let Constants = ../../../../schema/constants.dhall

in  { imagePullPolicy  = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , processor =
      { name  = "build-processor"
      , image = "build-processor:latest"
      }

    , proxyCACert = None Text
    }
