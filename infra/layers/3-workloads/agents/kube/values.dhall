-- infra/layers/3-workloads/agents/kube/values.dhall
--
-- Canonical defaults for the kubernetes agent chart.
-- Observer watches the cluster API and publishes to Cassini topics.
-- Consumer reads Cassini topics and writes to the graph.

let Constants = ../../../../schema/constants.dhall

in  { name            = "kube-agent"
    , imagePullPolicy = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , observer =
      { name               = "kube-observer"
      , image              = "polar-kube-observer:latest"
      , serviceAccountName = "kube-observer-sa"
      , secretName         = "kube-observer-sa-token"
      }

    , consumer =
      { name  = "kube-consumer"
      , image = "polar-kube-consumer:latest"
      }

    , proxyCACert = None Text
    }
