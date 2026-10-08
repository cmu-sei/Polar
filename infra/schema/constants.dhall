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
let kubernetes =
      ./kubernetes.dhall
        sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let Agents =
      ./agents.dhall
        sha256:8458543ac92aa19c950efdf97f5d89c85fbf3be94daad98d716c3217deddd570

let PolarNamespace = "polar"

let GraphNamespace = "polar-graph"
let AuthNamespace  = "polar-auth"

let tlsPath = "/etc/tls/certs"

let mtls =
      { commonName = "polar"
      , caCertPath = "${tlsPath}/ca.pem"
      , certPath = "${tlsPath}/cert.pem"
      , keyPath = "${tlsPath}/key.pem"
      , proxyCertificate = "proxy-ca-cert"
      }

-- The name cert-issuer's TLS certificate is issued for and the name every
-- caller connects to. Callers verify the server against the host in their
-- URL, so the two must be the same value.
let certIssuerHost = "cert-issuer.${PolarNamespace}.svc.cluster.local"

let certIssuerUrl = "https://${certIssuerHost}:8443"

let certVolumeName = "polar-certs"

let saTokenVolumeName = "sa-token"

let saTokenPath = "/workspace/token"

let cassiniPort = 8080

let cassiniService = { name = "cassini-ip-svc", type = "ClusterIP" }

let cassiniDNSName =
      "${cassiniService.name}.${PolarNamespace}.svc.cluster.local"

let cassiniAddr = "${cassiniDNSName}:${Natural/show cassiniPort}"

let CassiniServerCertificateSecret = "cassini-tls"

let neo4jConfigmapName = "neo4j-config"

let neo4jServiceName = "polar-db-svc"

let neo4jDNSName = "${neo4jServiceName}.${GraphNamespace}.svc.cluster.local"

let graphSecretName = "polar-graph-pw"

let neo4jSecret =
      kubernetes.SecretKeySelector::{
      , key = "secret"
      , name = Some "neo4j-secret"
      }

let neo4jClientCertVolumeName = "neo4j-client-tls"

let neo4jClientCertPath       = "/etc/neo4j-client-tls"

let neo4jClientSaTokenVolName = "neo4j-client-sa-token"

let graphSecretKeySelector =
      kubernetes.SecretKeySelector::{
      , name = Some "polar-graph-pw"
      , key = "secret"
      }

let graphConfig
    : Agents.GraphConfig
    = { graphDB = "neo4j"
      , graphUsername = "neo4j"
      , graphPassword = neo4jSecret
      }

let gitlabSecretKeySelector =
      kubernetes.SecretKeySelector::{
      , key = "token"
      , name = Some "gitlab-secret"
      }

let graphPassword =
      kubernetes.SecretKeySelector::{
      , name = Some "polar-graph-pw"
      , key = "secret"
      }

let RegistryResolverName = "oci-registry-resolver"

let ArtifactLinkerName = "artifact-linker"

let ProvenanceLinkerName = "provenance-linker"

let ProvenanceResolverName = "provenance-resolver"

let OciRegistrySecret = { name = "oci-registry-auth" }

let DropAllCapSecurityContext =
      kubernetes.SecurityContext::{
      , runAsGroup = Some 1000
      , runAsNonRoot = Some True
      , runAsUser = Some 1000
      , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
      }

let RejectSidecarAnnotation =
      { mapKey = "sidecar.istio.io/inject", mapValue = "false" }

let commonClientTls
    : Agents.ClientTlsConfig
    = { broker_endpoint = cassiniAddr
      , server_name = cassiniDNSName
      , client_certificate_path = mtls.certPath
      , client_key_path = mtls.keyPath
      , client_ca_cert_path = mtls.caCertPath
      }

let certEmptyDirVolume =
      kubernetes.Volume::{
      , name = certVolumeName
      , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
      }

let saTokenVolume =
      \(audience : Text) ->
        kubernetes.Volume::{
        , name = saTokenVolumeName
        , projected = Some kubernetes.ProjectedVolumeSource::{
          , sources = Some
            [ kubernetes.VolumeProjection::{
              , serviceAccountToken = Some kubernetes.ServiceAccountTokenProjection::{
                , path = "token"
                , audience = Some audience
                , expirationSeconds = Some 3600
                }
              }
            ]
          }
        }

let certVolumeMount =
      kubernetes.VolumeMount::{ name = certVolumeName, mountPath = tlsPath }

let commonClientEnv =
      [ kubernetes.EnvVar::{
        , name = "TLS_CA_CERT"
        , value = Some mtls.caCertPath
        }
      , kubernetes.EnvVar::{
        , name = "TLS_CLIENT_CERT"
        , value = Some mtls.certPath
        }
      , kubernetes.EnvVar::{
        , name = "TLS_CLIENT_KEY"
        , value = Some mtls.keyPath
        }
      , kubernetes.EnvVar::{ name = "BROKER_ADDR", value = Some cassiniAddr }
      , kubernetes.EnvVar::{
        , name = "CASSINI_SERVER_NAME"
        , value = Some cassiniDNSName
        }
      ]

-- When a Job replaces a pod. `Failed` waits until the old pod is fully gone. The default
-- (`TerminatingOrFailed`) creates the replacement the moment the old pod starts terminating,
-- so a deleted or evicted pod overlaps with its replacement for the whole drain. Every agent
-- ends a pod by exiting non-zero, so a pod always finishes `Failed` and this loses nothing.
-- See infra/INFRA.md for the one case it makes worse (a pod stuck terminating on a dead node).
let JobPodReplacementPolicy = "Failed"

in  { PolarNamespace
    , GraphNamespace
    , AuthNamespace
    , tlsPath
    , mtls
    , certIssuerHost
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
    , JobPodReplacementPolicy
    , commonClientTls
    , certEmptyDirVolume
    , saTokenVolume
    , certVolumeMount
    , commonClientEnv
    , neo4jClientCertVolumeName
    , neo4jClientCertPath
    , neo4jClientSaTokenVolName
    }
