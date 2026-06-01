-- lib-constants.dhall
--
-- Values that are fixed by Polar's architecture and will never vary across
-- deployments. These are safe for the public library files (agents.dhall,
-- cassini.dhall, functions.dhall) to import and close over.
--
-- DO NOT add anything here that:
--   - reads from an environment variable  (those belong in constants.dhall)
--   - embeds a file with `as Text`        (same)
--   - references a Kubernetes secret by name that a deployer controls
--   - is specific to local vs. staging vs. production
--
-- If you find yourself wanting to add something here that varies between
-- environments, it should be a parameter on the relevant type's `default`
-- record or on the constructor function, not a constant.

-- -------------------------------------------------------------------------
-- Namespace
-- -------------------------------------------------------------------------
-- Polar is a single-instance, cohesive observability sink. Two instances in
-- the same cluster are nonsensical. The namespace is not configurable.
let polarNamespace : Text = "polar"

-- -------------------------------------------------------------------------
-- Certificate paths
-- -------------------------------------------------------------------------
-- All Polar pods mount their mTLS certs at the same well-known directory.
-- The cert-client init container writes here; agents and Cassini read from
-- here. Changing this would require coordinated changes across every pod
-- spec and the cert-client binary itself.
let certDir : Text = "/home/polar/certs"

let certPaths =
      { ca   = "${certDir}/ca.pem"
      , cert = "${certDir}/cert.pem"
      , key  = "${certDir}/key.pem"
      }

-- -------------------------------------------------------------------------
-- Service account token
-- -------------------------------------------------------------------------
-- The projected SA token used by the cert-client init container to
-- authenticate against the cert-issuer. Path is fixed by the pod spec
-- convention across all agents.
let saTokenDir  : Text = "/home/polar/sa-token"
let saTokenFile : Text = "${saTokenDir}/token"

-- -------------------------------------------------------------------------
-- Cassini broker
-- -------------------------------------------------------------------------
-- Port assignments for Cassini. The HTTP port serves the health/metrics
-- endpoint; the TCP port is the mTLS broker. These are fixed by the
-- Cassini binary's defaults and the ClusterIP service spec.
let cassiniHttpPort : Natural = 3000
let cassiniTcpPort  : Natural = 8080

-- The server name Cassini presents in its TLS certificate. Agents use this
-- as the expected server name during the mTLS handshake. Fixed by the
-- cert-issuer configuration and the Cassini service account name.
let cassiniServerName : Text = "cassini.polar.serviceaccount.cluster.local"

-- The ClusterIP service name for Cassini. Combined with polarNamespace to
-- form the in-cluster DNS name that agents dial.
let cassiniServiceName : Text = "cassini-ip-svc"

let cassiniDNSName : Text =
      "${cassiniServiceName}.${polarNamespace}.svc.cluster.local"

let cassiniAddr : Text = "${cassiniDNSName}:${Natural/show cassiniTcpPort}"

-- -------------------------------------------------------------------------
-- Cert-client defaults
-- -------------------------------------------------------------------------
-- The audience string presented when requesting the projected SA token.
-- Fixed by the cert-issuer's RBAC configuration.
let certIssuerAudience : Text = "polar-cert-issuer.local"

-- The cert-issuer's in-cluster URL. Fixed by the cert-issuer Service name
-- and the polar namespace.
let certIssuerUrl : Text =
      "http://cert-issuer.${polarNamespace}.svc.cluster.local:8443"

let initScriptConfigMapName = "cert-init-script"

let saTokenVolumeName    = "sa-token"
let certVolumeName       = "polar-certs"
let initScriptVolumeName = "cert-init-script"
let certTokenExpiry      = 3600

in  { polarNamespace
    , certDir
    , certPaths
    , saTokenDir
    , saTokenFile
    , cassiniHttpPort
    , cassiniTcpPort
    , cassiniServerName
    , cassiniServiceName
    , cassiniDNSName
    , cassiniAddr
    , certIssuerAudience
    , certIssuerUrl
    , saTokenVolumeName
    , certVolumeName
    , initScriptVolumeName
    , initScriptConfigMapName
    , certTokenExpiry
    }
