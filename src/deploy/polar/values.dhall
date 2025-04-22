
let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0
let chart = ./chart.dhall
let namespace = "polar"
let sandboxHostSuffix = "sandbox.labz.s-box.org"

-- Values for pulling from a potentially private registry
let sandboxRegistry 
  = {
    url = "sandboxaksacr.azurecr.us"
    , imagePullSecrets = [
        kubernetes.LocalObjectReference::{ name = Some "sandbox-registry" }
    ]
  }

let gitRepoSecret = kubernetes.SecretReference::{ name = Some "flux-repo-secret", namespace = Some namespace }
-- The git repository FluxCD will look in for the latest version of the rendered chart.
let deployRepository = {
  name = "polar-deploy-repo"
  , spec 
    = { interval = "5m0s"
    , ref.branch = "sandbox"
    , url = "https://gitlab.sandbox.labz.s-box.org/sei/polar-deploy"
    , secretRef.name = gitRepoSecret.name
    }
}

-- Our test cluster has services that sit behind a service mesh. So we add an annotation to disable istio sidecar injection for our deployment.
-- Why? Polar is a self-contained framework desinged to operate without interference to ensure total privacy and minimum trust.
-- A service mesh runs against these goals. To work around this, we disable it and decide to instead treat it like any other proxy
-- in order to observe services behind it.
let RejectSidecarAnnotation = { mapKey = "sidecar.istio.io/inject", mapValue = "false" }

-- Settings for Polar's mTLS configurations
let tlsPath = "/etc/tls"

let mtls = {
,   commonName = "polar" -- TODO: Ascertain whether this is a desireable default CN
,   caCertificateIssuerName = "ca-issuer"
,   caCertificateRequest ="ca-certificate"
,   caCertName = "ca-cert"
,   leafIssuerName = "polar-leaf-issuer"
-- define shared ca cert paths for convnience
,   caCertPath = "${tlsPath}/ca.crt"
,   serverCertPath = "${tlsPath}/tls.crt"
,   serverKeyPath = "${tlsPath}/tls.key"
-- We're dealing with a service mesh, so we need to re-import a CA certificate to trust
,   proxyCertificate = "proxy-ca-cert"
}



let cassiniPort = 8080
let cassiniService = { name = "cassini-ip-svc", type = "ClusterIP" }

-- Predidcted DNS name for cassini once given a service
let cassiniDNSName = "${cassiniService.name}.${namespace}.svc.cluster.local"
let cassiniAddr = "${cassiniDNSName}:${Natural/show cassiniPort}"

let cassiniServerCertificateSecret = "cassini-tls"

let cassini = 
  {
    name = "cassini"
  , namespace = namespace
  , image = "${sandboxRegistry.url}/polar/cassini:${chart.appVersion}"
  , imagePullSecrets = sandboxRegistry.imagePullSecrets
  , containerSecurityContext 
    = kubernetes.SecurityContext::{
      , runAsGroup = Some 1000
      , runAsNonRoot = Some True
      , runAsUser = Some 1000
      , capabilities = Some kubernetes.Capabilities::{
        drop = Some [ "ALL" ]
      }
    }
    , podAnnotations = [ RejectSidecarAnnotation ]
  , port = cassiniPort
  , service = cassiniService
  , tls = {
      certificateRequestName = "cassini-certificate"
      , certificateSpec = { 
          , commonName = mtls.commonName
          , dnsNames = [ cassiniDNSName ]
          , duration = "2160h" -- 90 days by default
          , issuerRef = { kind = "Issuer", name = mtls.leafIssuerName }
          , renewBefore = "360h" -- 15 days
          , secretName = cassiniServerCertificateSecret
        }
  }
  , environment 
    = [
        kubernetes.EnvVar::{
            name = "TLS_CA_CERT"
            , value = Some mtls.caCertPath
        }
        , kubernetes.EnvVar::{
            name = "TLS_SERVER_CERT_CHAIN"
            , value = Some mtls.serverCertPath
        }
        , kubernetes.EnvVar::{
            name = "TLS_SERVER_KEY"
            , value = Some mtls.serverKeyPath
        }
        , kubernetes.EnvVar::{
            name = "CASSINI_BIND_ADDR"
            , value = Some "0.0.0.0:${Natural/show cassiniPort}"
        }
      ]
  , volumes = 
    [
      kubernetes.Volume::{
        , name = cassiniServerCertificateSecret
        , secret = Some kubernetes.SecretVolumeSource::{
            secretName = Some cassiniServerCertificateSecret
        }
      }
    ]
    , volumeMounts
       = [
        , kubernetes.VolumeMount::{
          name = cassiniServerCertificateSecret
          , mountPath = tlsPath
          , readOnly = Some True
        }
    ]
  }


-- The name of the secret containing the neo4j default credentials
let graphSecret = 
      kubernetes.SecretKeySelector::{
        key = "secret"
        , name = Some "neo4j-secret"
      }  
-- The secret used by the observers to contact gitlab
let gitlabSecret = kubernetes.SecretKeySelector::{ key = "token" , name = Some "gitlab-secret" }
-- The name of the secret containign the tls keypair
let gitlabClientCertificateSecret = "client-tls"
--  Certificate request spec for creating a client keypair
let gitlabAgentCertificateSpec 
    = { commonName = mtls.commonName
      , dnsNames = [ cassiniDNSName ]
      , duration = "2160h"
      , issuerRef = { kind = "Issuer", name = mtls.leafIssuerName }
      , renewBefore = "360h"
      , secretName = gitlabClientCertificateSecret
      }
--  General settings for the gitlab agent components
let gitlab = {
    name = "gitlab-agent"
    , serviceAccountName = "gitlab-agent-sa"
    , podAnnotations = [ RejectSidecarAnnotation ]
    , imagePullSecrets = sandboxRegistry.imagePullSecrets
    , containerSecurityContext 
      = kubernetes.SecurityContext::{
        , runAsGroup = Some 1000
        , runAsNonRoot = Some True
        , runAsUser = Some 1000
        , capabilities = Some kubernetes.Capabilities::{
          drop = Some [ "ALL" ]
        }
      }
    -- Here, you can provide the name of a proxy CA secret that'll be loaded into the pod using the proxyCertificate field.
    -- If you're working in Kubernetes + Istio world, dealing with its mTLS, routing, observability, and policy layers
    -- or just dealing with some other MITM situation, You'll need to trust the proxies certificates 
    , tls = {
      , certificateRequestName = "gitlab-agent-certificate"
      , certificateSpec = gitlabAgentCertificateSpec

    }    
    , observer = {
        name = "polar-gitlab-observer"
        , image = "${sandboxRegistry.url}/polar/polar-gitlab-observer:${chart.appVersion}"
        , gitlabEndpoint = "https://gitlab.sandbox.labz.s-box.org/api/graphql"
        , gitlabSecret = gitlabSecret
    }
    , consumer = {
        name = "polar-gitlab-consumer"
        , image = "${sandboxRegistry.url}/polar/polar-gitlab-consumer:${chart.appVersion}"
        -- Settings to configure the consumer's connection to the graph database
        
        , graph = {
             graphDB = "neo4j"
          ,  graphUsername = "neo4j"
          -- The secret containing JUST the password string used by the consumer to authenticate
          -- Not to be confused with the full crednetial string, which is formatted like  <username>/<password>
          ,  graphPassword = 
              kubernetes.SecretKeySelector::{
                name = Some "polar-graph-pw"
                , key = "secret"
              }
        }
    }
    }

let neo4jPorts = { https = 7473, bolt = 7687 }

let neo4jHomePath = "/var/lib/neo4j"

-- Configurations for our neo4j database statefulset
let neo4j =
      { name = "polar-neo4j"
      , hostName = "graph-db.sandbox.labz.s-box.org"
      -- neo4j is going to get some of its own resources, like volumes, certificates, policies, etc.
      -- so we're gonna seperate it.
      , namespace = "polar-db"
      -- TODO: make configurable, we'll want to use private registries
      , image = "${sandboxRegistry.url}/ironbank/opensource/neo4j/neo4j:5.26.2"
      , imagePullSecrets = sandboxRegistry.imagePullSecrets
      , config = { name = "neo4j-config" , path = "/var/lib/neo4j/conf" }
      , env =
          [ kubernetes.EnvVar::{
              name = "NEO4J_AUTH"
              , valueFrom = Some kubernetes.EnvVarSource::{ secretKeyRef = Some graphSecret }
            }
          ]
      , containerPorts =
          [ kubernetes.ContainerPort::{ containerPort = neo4jPorts.https }
          , kubernetes.ContainerPort::{ containerPort = neo4jPorts.bolt }
          ]
      , service = { name = "polar-db-svc" }      
      , logging = { serverLogsXml = "", userLogsXml = "" }
      -- Hardware resource requirements
      -- neo4j reccommends at least 2 vCPU cores and 2 GiB of memory for cloud deployments
      -- We can probbaly adjust up or down, later.
      -- REFERENCE: https://neo4j.com/docs/operations-manual/current/installation/requirements/
      , resources = { cpu = "2000m", memory = "2Gi" }
      , podSecurityContext = kubernetes.PodSecurityContext::{
        , fsGroup = Some 7474
        , fsGroupChangePolicy = Some "OnRootMismatch"
      }
      , containerSecurityContext =
          kubernetes.SecurityContext::{
          , runAsGroup = Some 7474
          , runAsNonRoot = Some True
          , runAsUser = Some 7474
          , capabilities = Some kubernetes.Capabilities::{ drop = Some [ "ALL" ] }
          }
      , tls = {
        caIssuerName = "selfsigned-root"
        , leafIssuer = "db-ca-issuer"
        , caSecretName = "root-ca-secret"
        , leafSecretName = "neo4j-keypair"
      }
      -- Here we define some volume configurations
      -- We're targeting an azure k8s cluster, so we'll use default managed-csi storage for now
      , volumes =
          { data =
              { name = "polar-db-data"
              , storageClassName = Some "managed-csi"
              , storageSize = "10Gi"
              , mountPath = "/var/lib/neo4j/data"
              }
          , logs =
              { name = "polar-db-logs"
              , storageClassName = Some "managed-csi"
              , storageSize = "10Gi"
              , mountPath = "/var/lib/neo4j/logs"
              }
          }
      }

let neo4jDNSName = "${neo4j.service.name}.${neo4j.namespace}.svc.cluster.local"
-- Point to the istio sidecar's VS
let neo4jBoltAddr = "neo4j://${neo4jDNSName}:7687"
let neo4jUiAddr = "${neo4jDNSName}:${Natural/show neo4jPorts.https}"

in

{   namespace
,   deployRepository
,   sandboxHostSuffix
,   sandboxRegistry
,   mtls
,   tlsPath
,   cassini
,   cassiniDNSName
,   cassiniAddr
,   neo4jPorts
,   neo4j
,   graphSecret
,   neo4jDNSName
,   neo4jUiAddr
,   neo4jBoltAddr
,   gitlab
,   gitlabSecret
}
