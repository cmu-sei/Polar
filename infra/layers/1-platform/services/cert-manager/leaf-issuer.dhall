let Constants = ../../../../schema/constants.dhall

in  { apiVersion = "cert-manager.io/v1"
    , kind       = "Issuer"
    , metadata   =
      { name      = Constants.mtls.leafIssuerName
      , namespace = Constants.PolarNamespace
      }
    , spec.ca.secretName = Constants.mtls.caCertName
    }
