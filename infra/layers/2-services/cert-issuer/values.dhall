-- infra/layers/2-services/cert-issuer/values.dhall

let Constants = ../../../schema/constants.dhall

let CaProvisioning = ../../../schema/ca-provisioning.dhall

in  { name             = "cert-issuer"
    , image            = "cert-issuer:latest"
    , imagePullPolicy  = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }
    , port             = 8443
    , caVolumeName     = "cert-issuer-ca"
    , caStorageClass   = "managed-csi"
    , caStorageSize    = "1Gi"
    , caCertPath = "/home/polar/ca/ca.crt"
    , caKeyPath  = "/home/polar/ca/ca.key"
    -- MUST be overridden per target in overrides.dhall -- an empty
    -- list fails config validation loudly at cert-issuer startup
    -- (ConfigError::EmptyIssuerList) rather than silently deploying
    -- a service that can validate nothing.
    , issuers =
        [] : List
          { issuerUrl : Text
          , audience : List Text
          , jwksUri : Optional Text
          , workloadIdentityClaim : Text
          , instanceBindingClaim : Text
          , allowedAlgorithms : List Text
          , jwksCacheTtlMinSecs : Natural
          , jwksCacheTtlMaxSecs : Natural
          }
    , defaultLifetimeSecs = 3600
    , serverLifetimeSecs  = 3600
    -- Account (user) certificates, such as the one loaded into a browser.
    -- One year: a new one is an admin action, not something the holder
    -- renews by themselves.
    , userLifetimeSecs    = 31536000
    -- DNS name cert-issuer mints into its own TLS server cert. The
    -- ".svc.cluster.local" suffix is hardcoded, same as
    -- constants.dhall's certIssuerUrl; both are covered by the
    -- pending configurable-domain work.
    , mtlsServerIdentity = Constants.certIssuerHost
    -- Identities allowed to reach /internal/issue, matched against the
    -- verified mTLS peer SAN. Only the credentials agent: it is the one
    -- service allowed to ask for capability-bearing, account-bound
    -- certificates, and only after its ledger-backed checks pass. The
    -- ".serviceaccount.cluster.local" suffix is hardcoded, like
    -- mtlsServerIdentity above, pending the configurable-domain work.
    , mtlsInternalCallers =
        [ "credentials-agent.${Constants.PolarNamespace}.serviceaccount.cluster.local" ]
    -- Stays SelfBootstrap until bootstrap-ca exists and the Deployment
    -- mounts its Secret. Switching this without that mount makes
    -- cert-issuer refuse to start, by design.
    , caProvisioning = CaProvisioning.SelfBootstrap
    }
