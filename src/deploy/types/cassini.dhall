-- types/cassini.dhall
{-
  Describes the full configuration needed to deploy Cassini, the mTLS
  message broker at the center of the Polar system.

  Cassini is the only component that acts as a TLS *server* — all agents
  connect to it as clients. Its cert is issued by the cert-client at pod
  startup using a projected SA token, exactly like agent client certs, but
  with --cert-type server so the issued cert carries serverAuth EKU.

  Agents verify Cassini's server cert against the shared CA. Cassini
  verifies agent client certs against the same CA via WebPkiClientVerifier.
  One CA, one trust anchor for the whole system.
-}
let kubernetes = ./kubernetes.dhall

let Agents = ./agents.dhall

let Constants = ./constants.dhall

let CassiniTlsConfig =
      { ca_cert_path : Text, server_cert_path : Text, server_key_path : Text }

let Cassini =
      { name : Text
      , image : Text
      , ports : { http : Natural, tcp : Natural }
      , serviceAccountName : Text
      , certClient : Agents.CertClientConfig
      , tls : CassiniTlsConfig
      }

let defaults =
      { name = "cassini"
      , ports = { http = 3000, tcp = 8080 }
      , tls =
        { ca_cert_path = "${Constants.certDir}/ca.pem"
        , server_cert_path = "${Constants.certDir}/cert.pem"
        , server_key_path = "${Constants.certDir}/key.pem"
        }
        , serviceAccountName = "cassini"   -- was "cassini-sa"
      }

in  { Type = Cassini, defaults, CassiniTlsConfig }
