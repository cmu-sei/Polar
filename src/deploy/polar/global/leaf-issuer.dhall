let values = ../values.dhall

let leafIssuer 
      = { apiVersion = "cert-manager.io/v1"
      , kind = "Issuer"
      , metadata = { name = values.mtls.leafIssuerName , namespace = values.namespace}
      , spec = { ca = { secretName = values.mtls.caCertName } }
      }

in leafIssuer