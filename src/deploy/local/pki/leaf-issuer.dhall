let Constants = ../../types/constants.dhall

let leafIssuer
      = { apiVersion = "cert-manager.io/v1"
      , kind = "Issuer"
      , metadata = { name = Constants.mtls.leafIssuerName , namespace = Constants.PolarNamespace}
      , spec = { ca = { secretName = Constants.mtls.caCertName } }
      }

in leafIssuer
