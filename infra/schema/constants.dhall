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

let tlsPath = "/etc/tls"

let mtls =
      { commonName              = "polar"
      , caCertificateIssuerName = "ca-issuer"
      , caCertificateRequest    = "ca-certificate"
      , caCertName              = "ca-cert"
      , leafIssuerName          = "polar-leaf-issuer"
      , caCertPath              = "${tlsPath}/ca.crt"
      , certPath                = "${tlsPath}/tls.crt"
      , keyPath                 = "${tlsPath}/tls.key"
      , proxyCertificate        = "proxy-ca-cert"
      }

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

let gitObserverSecretName = "git-observer-secret"

let RegistryResolverName   = "oci-registry-resolver"
let ArtifactLinkerName     = "artifact-linker"
let ProvenanceLinkerName   = "provenance-linker"
let ProvenanceResolverName = "provenance-resolver"

-- Secret name only — the value (DOCKER_AUTH_JSON) is managed in secrets/shared/
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

let polarAgentCertificateSpec =
      { commonName  = mtls.commonName
      , dnsNames    = [ cassiniDNSName ]
      , duration    = "2160h"
      , issuerRef   = { kind = "Issuer", name = mtls.leafIssuerName }
      , renewBefore = "360h"
      , secretName  = "cassini-tls"
      }

-- =============================================================================
-- Common volumes and env vars
-- Mounted / injected into every agent pod.
-- =============================================================================

let ClientTlsVolume =
      kubernetes.Volume::{
      , name   = "client-tls"
      , secret = Some kubernetes.SecretVolumeSource::{
          , secretName = Some "client-tls"
          }
      }

let ClientTlsVolumeMount =
      kubernetes.VolumeMount::{
      , name      = CassiniServerCertificateSecret
      , mountPath = tlsPath
      }

-- Volumes mounted by every agent
let commonVolumes = [ ClientTlsVolume ]

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
    , gitObserverSecretName
    , RegistryResolverName
    , ArtifactLinkerName
    , ProvenanceLinkerName
    , ProvenanceResolverName
    , OciRegistrySecret
    , DropAllCapSecurityContext
    , RejectSidecarAnnotation
    , commonClientTls
    , polarAgentCertificateSpec
    , ClientTlsVolume
    , ClientTlsVolumeMount
    , commonVolumes
    , commonClientEnv
    }
