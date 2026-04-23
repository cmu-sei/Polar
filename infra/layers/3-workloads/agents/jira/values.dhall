-- infra/layers/3-workloads/agents/jira/values.dhall
--
-- Canonical defaults for the jira agent chart.
-- Observer polls Jira REST API and publishes to Cassini topics.
-- Processor reads Cassini topics and writes to the graph.
--
-- Required secrets (in secrets/workloads/agents/jira/):
--   jira-secret: contains JIRA_TOKEN

let Constants = ../../../../schema/constants.dhall

in  { name            = "jira-agents"
    , imagePullPolicy = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , observer =
      { name    = "jira-observer"
      , image   = "jira-observer:latest"
      , jiraUrl = "https://jira.example.com"
      }

    , processor =
      { name  = "jira-processor"
      , image = "jira-processor:latest"
      }

    , tls =
      { certificateRequestName = "jira-agent-certificate"
      , certificateSpec =
        { commonName  = Constants.mtls.commonName
        , dnsNames    = [ Constants.cassiniDNSName ]
        , duration    = "2160h"
        , issuerRef   = { kind = "Issuer", name = Constants.mtls.leafIssuerName }
        , renewBefore = "360h"
        , secretName  = "jira-agent-tls"
        }
      }

    , proxyCACert = None Text
    }
