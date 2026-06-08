{-
  values.dhall — local development deployment values

  This file defines the concrete values for a local development deployment.
  It replaces the previous cert-manager-based TLS model with the cert-issuer
  init container model. Key changes from the previous version:

  - All cert-manager TLS fields removed. Certs are now issued at pod startup
    by cert-issuer-init using projected SA tokens.
  - Every agent now carries certClient = defaultCertClientConfig.
    Override cert_type to "server" for Cassini only.
  - WithServiceAccount is only for agents that need Kubernetes API access
    (kubeObserver, buildOrchestrator). It is no longer used for TLS.
  - commonClientTls now points at certDir paths written by cert-issuer-init
    rather than cert-manager secret mounts.
  - cassiniServerName is the normalized SA identity DNS form.

  Environment variables required at render time:
    CI_COMMIT_SHORT_SHA  — image tag (defaults to "latest")
    GITLAB_TOKEN         — GitLab API token
    GRAPH_PASSWORD       — Neo4j password
    DOCKER_AUTH_JSON     — OCI registry auth JSON
-}
let kubernetes = ../types/kubernetes.dhall

let C = ../types/lib-constants.dhall

let Cassini = ../types/cassini.dhall

let Agents = ../types/agents.dhall

let commitSha = env:CI_COMMIT_SHORT_SHA as Text ? "latest"

let imagePullPolicy = "Never"

let img = \(name : Text) -> "${name}:${commitSha}"

-- -------------------------------------------------------------------------
-- Deployment-local constants
-- These are specific to this environment and do not belong in lib-constants.
-- -------------------------------------------------------------------------

let neo4jServiceName = "neo4j"
let neo4jDNSName     = "${neo4jServiceName}.${C.polarNamespace}.svc.cluster.local"

-- Well-known agent names used for deployment metadata and graph labeling.
let OciResolverName = "oci-resolver"

-- Graph config is deployment-specific: the DB name, user, and the
-- Kubernetes secret that holds the password are all decided by the deployer.
let graphPasswordSecret =
      kubernetes.SecretKeySelector::{
      , name = Some "polar-graph-pw"
      , key  = "secret"
      }

let graphConfig
    : Agents.GraphConfig
    = { graphDB       = "neo4j"
      , graphUsername = "neo4j"
      , graphPassword = graphPasswordSecret
      }

-- -------------------------------------------------------------------------
-- Shared TLS and cert-client config
-- Derived from lib-constants; every agent uses these unless overridden.
-- -------------------------------------------------------------------------

let tls
    : Agents.ClientTlsConfig
    = { broker_endpoint          = C.cassiniAddr
      , server_name              = C.cassiniServerName
      , client_certificate_path  = C.certPaths.cert
      , client_key_path          = C.certPaths.key
      , client_ca_cert_path      = C.certPaths.ca
      }

-- Default cert-client config for all agents. Uses the schema default
-- from agents.dhall (which is already populated from lib-constants),
-- so this is an explicit passthrough for readability.
let defaultCertClientConfig = Agents.CertClientConfig.default

-- -------------------------------------------------------------------------
-- Infrastructure components
-- -------------------------------------------------------------------------

let certIssuer
    : Agents.CertIssuer
    = { name               = "cert-issuer"
      , image              = img "cert-issuer"
      , serviceAccountName = "cert-issuer-sa"
      , config             = ./conf/issuer-config.json as Text
      }

let cassini
    : Cassini.Type
    =     Cassini.defaults
      //  { name               = "cassini"
          , image              = img "cassini"
          , serviceAccountName = "cassini"
          , certClient         = defaultCertClientConfig // { cert_type = "server" }
          }

let neo4j =
      { name            = "polar-neo4j"
      , hostName        = "neo4j"
      , namespace       = C.polarNamespace
      , image           = "neo4j:5.26.2"
      , configVolume    = "neo4j-config-copy"
      , certificatesVolume = "neo4j-certificates"
      , confDir         = "/var/lib/neo4j/conf"
      , certDir         = "/var/lib/neo4j/certificates"
      , ports           = { http = 7474, https = 7473, bolt = 7687 }
      , config          = { name = "neo4j-config", path = "/var/lib/neo4j/conf" }
      , enableTls       = True
      , volumes =
        { data =
          { name             = "polar-db-data"
          , storageClassName = Some "managed-csi"
          , storageSize      = "10Gi"
          , mountPath        = "/var/lib/neo4j/data"
          }
        , logs =
          { name             = "polar-db-logs"
          , storageClassName = Some "managed-csi"
          , storageSize      = "10Gi"
          , mountPath        = "/var/lib/neo4j/logs"
          }
        , certs =
          { name             = "polar-db-certs"
          , storageClassName = Some "managed-csi"
          , storageSize      = "1Gi"
          , mountPath        = "/var/lib/neo4j/certificates"
          }
        }
      }

let neo4jBoltAddr =
      "bolt+s://${neo4jDNSName}:${Natural/show neo4j.ports.bolt}"

let jaeger =
      { name  = "jaeger"
      , image = "cr.jaegertracing.io/jaegertracing/jaeger:2.13.0"
      , ports = { http = 16686, grpc = 14250, thrift = 6831 }
      , service.name = "jaeger-svc"
      }

let jaegerDNSName =
      "http://${jaeger.service.name}.${C.polarNamespace}.svc.cluster.local:${Natural/show jaeger.ports.http}/v1/traces"

-- -------------------------------------------------------------------------
-- Agents
-- -------------------------------------------------------------------------

let gitlabObserver
    : Agents.GitlabObserver
    = { name             = "polar-gitlab-observer"
      , image            = img "polar-gitlab-observer"
      , endpoint         = "https://gitlab.sandbox.labz.s-box.org"
      , maxBackoffSecs   = 300
      , baseIntervalSecs = 30
      , token            = Some env:GITLAB_TOKEN as Text
      , certClient       = defaultCertClientConfig
      , tls
      }

let gitlabConsumer
    : Agents.GitlabConsumer
    = { name     = "polar-gitlab-consumer"
      , image    = img "polar-gitlab-consumer"
      , graph    = graphConfig
      , certClient = defaultCertClientConfig
      , tls
      }

let resolver
    : Agents.OciResolver
    = { name     = OciResolverName
      , image    = img "oci-resolver"
      , certClient = defaultCertClientConfig
      , tls
      }

let gitObserver
    : Agents.GitObserver
    = { name     = "git-repo-observer"
      , image    = img "git-observer"
      , config   = ./conf/git.json as Text
      , certClient = defaultCertClientConfig
      , tls
      }

let gitConsumer
    : Agents.GitConsumer
    = { name     = "git-repo-consumer"
      , image    = img "git-processor"
      , graph    = graphConfig
      , certClient = defaultCertClientConfig
      , tls
      }

let gitScheduler
    : Agents.GitScheduler
    = { name     = "git-scheduler"
      , image    = img "git-processor"
      , graph    = graphConfig
      , certClient = defaultCertClientConfig
      , tls
      }

let kubeObserver
    : Agents.KubeObserver
    = { name               = "kube-observer"
      , image              = img "kube-observer"
      , serviceAccountName = "kube-observer-sa"
      , secretName         = "kube-observer-sa-token"
      , certClient         = defaultCertClientConfig
      , tls
      }

let kubeConsumer
    : Agents.KubeConsumer
    = { name     = "kube-consumer"
      , image    = img "kube-consumer"
      , graph    = graphConfig
      , certClient = defaultCertClientConfig
      , tls
      }

let buildProcessor
    : Agents.BuildProcessor
    = { name     = "build-processor"
      , image    = img "build-processor"
      , graph    = graphConfig
      , certClient = defaultCertClientConfig
      , tls
      }

let proxyCACert = None Text

in  { imagePullPolicy
    , certIssuer
    , cassini
    , gitlabObserver
    , gitlabConsumer
    , kubeObserver
    , kubeConsumer
    , resolver
    , gitObserver
    , gitConsumer
    , gitScheduler
    , neo4j
    , neo4jDNSName
    , neo4jBoltAddr
    , neo4jServiceName
    , jaeger
    , jaegerDNSName
    , buildProcessor
    , proxyCACert
    }
