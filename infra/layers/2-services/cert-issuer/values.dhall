-- infra/layers/2-services/cert-issuer/values.dhall

let Constants = ../../../schema/constants.dhall

in  { name             = "cert-issuer"
    , image            = "polar-cert-issuer:latest"
    , certClientImage  = "polar-cert-client:latest"
    , imagePullPolicy  = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }
    , port             = 8443
    , caVolumeName     = "cert-issuer-ca"
    , caStorageClass   = "managed-csi"
    , caStorageSize    = "1Gi"
    , caCertPath       = "/home/polar/ca.crt"
    , caKeyPath        = "/home/polar/ca.key"
    -- These two MUST be overridden per target in overrides.dhall:
    , oidcIssuerUrl    = ""
    , oidcAudience     = ""
    , oidcJwksUri      = None Text
    }
