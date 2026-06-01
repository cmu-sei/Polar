-- infra/layers/3-workloads/agents/gitlab/values.dhall
--
-- Canonical defaults for the gitlab agent chart.
-- Observer polls GitLab API and publishes to Cassini topics.
-- Consumer reads Cassini topics and writes to the graph.

let Constants = ../../../../schema/constants.dhall

in  { name            = "gitlab-agents"
    , imagePullPolicy = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , observer =
      { name     = "polar-gitlab-observer"
      , image    = "polar-gitlab-observer:latest"
      , endpoint = "https://gitlab.example.com"
      , baseIntervalSecs = 30
      , maxBackoffSecs   = 300
      }

    , consumer =
      { name  = "polar-gitlab-consumer"
      , image = "polar-gitlab-consumer:latest"
      }

    -- proxyCACert: set to Some "proxy-ca-cert" if a proxy sits in front of GitLab
    , proxyCACert = None Text
    }
