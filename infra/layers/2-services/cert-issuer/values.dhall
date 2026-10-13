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
    -- The issuance floor (docs/architecture/cert-issuer-isolation.md): the
    -- only workloads that may be issued a certificate with nothing but a
    -- service-account token, because they are what the credentials agent
    -- needs in order to run. Every other workload is to be issued through
    -- the credentials agent, whose mTLS certificate vouches for it. Each
    -- bootstrap workload is written as its token names it, so no cluster
    -- domain appears; the one server name is the same constant Cassini's own
    -- certificate request is built from.
    , mtlsBootstrap =
        [ { workload = "system:serviceaccount:${Constants.PolarNamespace}:credentials-agent"
          , certType = "CLIENT"
          , names = [] : List Text
          , vouchedOnly = False
          }
        , { workload = "system:serviceaccount:${Constants.PolarNamespace}:credentials-agent-observer"
          , certType = "CLIENT"
          , names = [] : List Text
          , vouchedOnly = False
          }
        , { workload = "system:serviceaccount:${Constants.PolarNamespace}:cassini"
          , certType = "SERVER"
          , names = [ Constants.cassiniDNSName ]
          , vouchedOnly = False
          }
        -- Workloads reached only through the credentials agent. They are listed
        -- so cert-issuer checks the names the credentials agent states for them
        -- (a token alone never gets them a certificate). Neo4j's list is the
        -- one its init container asks for (`neo4jSans` in the target's
        -- overrides); the catalog renders both from one place in phase 5.
        , { workload = "system:serviceaccount:${Constants.GraphNamespace}:graph"
          , certType = "SERVER"
          , names =
            [ "neo4j"
            , "polar-neo4j"
            , Constants.neo4jServiceName
            , "${Constants.neo4jServiceName}.${Constants.GraphNamespace}"
            , Constants.neo4jDNSName
            , "localhost"
            ]
          , vouchedOnly = True
          }
        , { workload = "system:serviceaccount:${Constants.PolarNamespace}:polar-git-server"
          , certType = "SERVER"
          , names = [ "polar-git-server.${Constants.PolarNamespace}.svc.cluster.local" ]
          , vouchedOnly = True
          }
        ]
    -- Whether cert-issuer enforces the floor. False is the old open
    -- behaviour (any token holder is signed what it asks for), and
    -- cert-issuer says so at every start. It is turned on in the same change
    -- that has polar-init request its certificate through the credentials
    -- agent: until then every other workload still asks cert-issuer
    -- directly, and enforcing would stop them all from starting.
    , mtlsEnforceBootstrap = False
    -- Stays SelfBootstrap until bootstrap-ca exists and the Deployment
    -- mounts its Secret. Switching this without that mount makes
    -- cert-issuer refuse to start, by design.
    , caProvisioning = CaProvisioning.SelfBootstrap
    }
