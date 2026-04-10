-- infra/layers/2-services/cassini/server-cert.dhall
--
-- Cassini server TLS certificate (cert-manager Certificate resource).
-- Pure function: receives the tls config from values, produces the Certificate.

let Constants = ../../../schema/constants.dhall

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
          { name      = "cassini-server-certificate"
          , namespace = Constants.PolarNamespace
          }
        , spec = tls.certificateSpec
        }

in render
