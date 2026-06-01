-- infra/schema/constants.dhall
--
-- Truly static constants: names, ports, paths, labels, selectors, volumes.
-- No environment variables. No constructed resources. No target-specific values.
--
-- If you find yourself wanting to add something here, ask:
--   env-dependent value?      → thread through render.nu at render time
--   constructed k8s resource? → belongs in the chart that owns it
--   target-specific value?    → targets/<target>/overrides.dhall
--   secret?                   → secrets/<layer>/<chart>/

let kubernetes = ./kubernetes.dhall
let Agents     = ./agents.dhall

-- =============================================================================
-- Namespaces
-- =============================================================================

let PolarNamespace = "polar"
let GraphNamespace = "polar-graph"

-- =============================================================================
-- TLS / mTLS
-- =============================================================================
-- Paths match cert-client defaults: --cert-dir /etc/tls/certs

let tlsPath = "/etc/tls/certs"

let mtls =
      { commonName  = "polar"
      , caCertPath  = "${tlsPath}/ca.pem"
      , certPath    = "${tlsPath}/cert.pem"
      , keyPath     = "${tlsPath}/key.pem"
      , proxyCertificate = "proxy-ca-cert"
      }

-- =============================================================================
-- Cert-issuer
-- =============================================================================
-- The cert-client init container talks to this URL to obtain its certificate.
-- Matches the cert-issuer Service name and port defined in 2-services/cert-issuer/.

let certIssuerUrl =
      "http://cert-issuer.${PolarNamespace}.svc.cluster.local:8443"

-- Volume and mount names used by every agent pod's cert-client init container.
let certVolumeName    = "polar-certs"
let saTokenVolumeName = "sa-token"
-- Path where the projected SA token is mounted — matches cert-client default.
let saTokenPath = "/workspace/token"

-- =============================================================================
-- Cassini
-- =============================================================================

let cassiniPort    = 8080
let cassiniService = { name = "cassini-ip-svc", type = "ClusterIP" }

let cassiniDNSName =
      "${cassiniService.name}.${PolarNamespace}.svc.cluster.local"

let cassiniAddr = "${cassiniDNSName}:${Natural/show cassiniPort}"

let CassiniServerCertificateSecret = "cassini-tls"

-- =============================================================================
-- Neo4j
-- =============================================================================

let neo4jConfigmapName = "neo4j-config"
let neo4jServiceName   = "polar-db-svc"
let neo4jDNSName       = "${neo4jServiceName}.${GraphNamespace}.svc.cluster.local"
let graphSecretName    = "polar-graph-pw"

let neo4jSecret =
      kubernetes.SecretKeySelector::{
      , key  = "secret"
      , name = Some "neo4j-secret"
      }

let graphSecretKeySelector =
      kubernetes.SecretKeySelector::{
      , name = Some "polar-graph-pw"
      , key  = "secret"
      }

-- =============================================================================
-- Graph config
-- =============================================================================

let graphConfig
    : Agents.GraphConfig
    = { graphDB       = "neo4j"
      , graphUsername = "neo4j"
      , graphPassword = neo4jSecret
      }

-- =============================================================================
-- Agent / workload constants
-- =============================================================================

let gitlabSecretKeySelector =
      kubernetes.SecretKeySelector::{
      , key  = "token"
      , name = Some "gitlab-secret"
      }

let graphPassword =
      kubernetes.SecretKeySelector::{
      , name = Some "polar-graph-pw"
      , key  = "secret"
      }

let RegistryResolverName   = "oci-registry-resolver"
let ArtifactLinkerName     = "artifact-linker"
let ProvenanceLinkerName   = "provenance-linker"
let ProvenanceResolverName = "provenance-resolver"

let OciRegistrySecret = { name = "oci-registry-auth" }

-- =============================================================================
-- Security contexts
-- =============================================================================

let DropAllCapSecurityContext =
      kubernetes.SecurityContext::{
      , runAsGroup   = Some 1000
      , runAsNonRoot = Some True
      , runAsUser    = Some 1000
      , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
      }

let RejectSidecarAnnotation =
      { mapKey = "sidecar.istio.io/inject", mapValue = "false" }

-- =============================================================================
-- Common TLS client config
-- Consumed by every agent that connects to Cassini.
-- =============================================================================

let commonClientTls
    : Agents.ClientTlsConfig
    = { broker_endpoint         = cassiniAddr
      , server_name             = cassiniDNSName
      , client_certificate_path = mtls.certPath
      , client_key_path         = mtls.keyPath
      , client_ca_cert_path     = mtls.caCertPath
      }

-- =============================================================================
-- Common volumes
-- Every agent pod mounts these two volumes: one emptyDir for the cert bundle
-- written by the cert-client init container, one projected SA token for the
-- init container to authenticate against the cert-issuer.
-- =============================================================================

let certEmptyDirVolume =
      kubernetes.Volume::{
      , name      = certVolumeName
      , emptyDir  = Some kubernetes.EmptyDirVolumeSource::{=}
      }

let saTokenVolume =
      \(audience : Text) ->
        kubernetes.Volume::{
        , name = saTokenVolumeName
        , projected = Some kubernetes.ProjectedVolumeSource::{
          , sources = Some
            [ kubernetes.VolumeProjection::{
              , serviceAccountToken = Some kubernetes.ServiceAccountTokenProjection::{
                , path     = "token"
                , audience = Some audience
                , expirationSeconds = Some 3600
                }
              }
            ]
          }
        }

let certVolumeMount =
      kubernetes.VolumeMount::{
      , name      = certVolumeName
      , mountPath = tlsPath
      }

-- =============================================================================
-- Common env vars injected into every agent container
-- =============================================================================

let commonClientEnv =
      [ kubernetes.EnvVar::{ name = "TLS_CA_CERT",         value = Some mtls.caCertPath  }
      , kubernetes.EnvVar::{ name = "TLS_CLIENT_CERT",     value = Some mtls.certPath    }
      , kubernetes.EnvVar::{ name = "TLS_CLIENT_KEY",      value = Some mtls.keyPath     }
      , kubernetes.EnvVar::{ name = "BROKER_ADDR",         value = Some cassiniAddr      }
      , kubernetes.EnvVar::{ name = "CASSINI_SERVER_NAME", value = Some cassiniDNSName   }
      ]

in  { PolarNamespace
    , GraphNamespace
    , tlsPath
    , mtls
    , certIssuerUrl
    , certVolumeName
    , saTokenVolumeName
    , saTokenPath
    , cassiniPort
    , cassiniService
    , cassiniDNSName
    , cassiniAddr
    , CassiniServerCertificateSecret
    , neo4jConfigmapName
    , neo4jServiceName
    , neo4jDNSName
    , graphSecretName
    , neo4jSecret
    , graphSecretKeySelector
    , graphConfig
    , gitlabSecretKeySelector
    , graphPassword
    , RegistryResolverName
    , ArtifactLinkerName
    , ProvenanceLinkerName
    , ProvenanceResolverName
    , OciRegistrySecret
    , DropAllCapSecurityContext
    , RejectSidecarAnnotation
    , commonClientTls
    , certEmptyDirVolume
    , saTokenVolume
    , certVolumeMount
    , commonClientEnv
    }
