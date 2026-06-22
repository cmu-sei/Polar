let Constants = ../../../../schema/constants.dhall

in  { apiVersion = "cert-manager.io/v1"
    , kind       = "Issuer"
    , metadata   =
      { name      = Constants.mtls.caCertificateIssuerName
      , namespace = Constants.PolarNamespace
      }
    , spec.selfSigned = {=}
    }
