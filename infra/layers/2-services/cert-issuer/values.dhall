-- ACE Tool
--
-- Copyright 2026 Carnegie Mellon University.
--
-- NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
-- FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
-- EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
-- PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
-- MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
-- TRADEMARK, OR COPYRIGHT INFRINGEMENT.
--
-- Licensed under a MIT (SEI)-style license, please see license.txt or contact
-- permission@sei.cmu.edu for full terms.
--
-- [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
-- distribution.  Please see Copyright notice for non-US Government use and distribution.
--
-- This Software includes and/or makes use of Third-Party Software each subject to its own license.
--
-- DM26-0670

-- infra/layers/2-services/cert-issuer/values.dhall

let Constants = ../../../schema/constants.dhall

let CaProvisioning = ../../../schema/ca-provisioning.dhall

in  { name             = "cert-issuer"
    , image            = "cert-issuer:latest"
    , imagePullPolicy  = "Always"
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
    -- An agent's escrow certificate: its durable encryption identity, which outlives its pods and is never issued
    -- twice for an identity. One year, until revocation is built and the lifetime is revisited.
    , serviceLifetimeSecs = 31536000
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
    -- The issuance floor (docs/architecture/cert-issuer-isolation.md): who may
    -- be signed what. Only cert-issuer's own entry is written here: `infra/resolve.nu floor` writes
    -- the entries for every catalog workload from what it is issued (the
    -- bootstrap identities, which may be signed on a token alone, and the
    -- workloads on the credentials agent that are issued a server
    -- certificate), and main.nu adds Neo4j's, from its chart and the target's
    -- `neo4jSans`. Rendering puts them in front of whatever is listed here.
    -- cert-issuer's own entry: it is a bootstrap service too, and signs itself its client certificate on its own
    -- token, through this floor (docs/architecture/cert-issuer-isolation.md, "1b in detail"). Its group is the one
    -- every Cassini client is in; it declares no topics of its own. Written here until cert-issuer is a catalog
    -- workload, whose entry the catalog writes like every bootstrap service's.
    , mtlsBootstrap =
        [ { workload = "system:serviceaccount:${Constants.PolarNamespace}:cert-issuer"
          , certType = "CLIENT"
          , names = [] : List Text
          , vouchedOnly = False
          , publish = [] : List Text
          , subscribe = [] : List Text
          , groups = [ "cassini-client" ]
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
