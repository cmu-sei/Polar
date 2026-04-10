-- infra/layers/3-workloads/agents/jira/agent-cert.dhall

let Constants = ../../../../schema/constants.dhall

let render =
      \(tls :
          { certificateRequestName : Text
          , certificateSpec :
            { commonName  : Text
            , dnsNames    : List Text
            , duration    : Text
            , issuerRef   : { kind : Text, name : Text }
            , renewBefore : Text
            , secretName  : Text
            }
          }
      ) ->
        { apiVersion = "cert-manager.io/v1"
        , kind       = "Certificate"
        , metadata   =
          { name      = tls.certificateRequestName
          , namespace = Constants.PolarNamespace
          }
        , spec = tls.certificateSpec
        }

in render
