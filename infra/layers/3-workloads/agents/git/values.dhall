-- infra/layers/3-workloads/agents/git/values.dhall
--
-- Canonical defaults for the git agent chart.
-- Three containers: observer (watches repos), consumer (processes commits),
-- scheduler (dispatches ad-hoc repo observation tasks).
--
-- git.json config is read from targets/<target>/conf/git.json by render.nu
-- and injected as a Secret at render time.

let Constants = ../../../../schema/constants.dhall

in  { name            = "git-agents"
    , imagePullPolicy = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , observer =
      { name  = "git-repo-observer"
      , image = "polar-git-repo-observer:latest"
      }

    , consumer =
      { name  = "git-repo-consumer"
      , image = "polar-git-consumer:latest"
      }

    , scheduler =
      { name  = "git-scheduler"
      , image = "polar-git-scheduler:latest"
      }

    , tls =
      { certificateRequestName = "git-agent-certificate"
      , certificateSpec =
        { commonName  = Constants.mtls.commonName
        , dnsNames    = [ Constants.cassiniDNSName ]
        , duration    = "2160h"
        , issuerRef   = { kind = "Issuer", name = Constants.mtls.leafIssuerName }
        , renewBefore = "360h"
        , secretName  = "git-agent-tls"
        }
      }

    , proxyCACert = None Text
    }
