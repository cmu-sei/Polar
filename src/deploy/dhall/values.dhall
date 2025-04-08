
let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0
let chart = ./chart.dhall
let namespace = "polar"

let imagePullSecrets = [
    { name = "sandbox-registry" }
]

let RejectSidecarAnnotation = { mapKey = "sidecar.istio.io/inject", mapValue = "false" }

-- Settings for Polar's mTLS configurations
let mtls = {
,   commonName = "polar" -- TODO: This value is hard-coded into the listenerManager. Time we changed that?
,   caCertificateIssuerName = "polar-ca-issuer"
,   caCertName = "polar-ca-cert"
,   leafIssuerName = "polar-leaf-issuer"
,   cassini.secretName = "cassini-tls"
,   gitlab.secretName = "gitlab-agent-tls"
}

let cassiniPort = 8080
let cassiniService = { name = "cassini-ip-svc", type = "ClusterIP" }

-- Predidcted DNS name for cassini once given a service
let cassiniAddr = "${cassiniService.name}.${namespace}.svc.cluster.local:${Natural/show cassiniPort}"

let cassini = 
  {
    name = "cassini"
  , namespace = "polar"
  , image = "localhost/cassini:${chart.appVersion}"
  , podAnnotations = [ RejectSidecarAnnotation ]
  , port = cassiniPort
  , service = cassiniService
  , mtls.certificateSpec = { 
        commonName = mtls.commonName
        , dnsNames = [ cassiniAddr ]
        , duration = "2160h" -- 90 days by default
        , issuerRef = { kind = "ClusterIssuer", name = mtls.leafIssuerName }
        , renewBefore = "360h" -- 15 days
        , secretName = mtls.cassini.secretName
        }
  , volumes = 
    [
    , kubernetes.Volume::{
        , name = "mtls-secrets"
        , secret = Some kubernetes.SecretVolumeSource::{
            secretName = Some mtls.cassini.secretName
        }
      }
    ]
  }



let graphSecret = 
      kubernetes.SecretKeySelector::{
        key = "secret"
        , name = Some "neo4j-secret"
      }  

let gitlab = {
    name = "gitlab-agent"
    , serviceAccountName = "gitlab-agent-sa"
    , podAnnotations = [ RejectSidecarAnnotation ]
    -- Optionally provide the name of a proxy CA secret
    -- , ProxyCertificate = None Text
    , proxyCertificate = Some "istio-ca-root-cert"


    , mtls.certificateSpec = 
      { commonName = mtls.commonName
        , dnsNames = [ cassiniAddr ]
        , duration = "2160h"
        , issuerRef = { kind = "ClusterIssuer", name = mtls.leafIssuerName }
        , renewBefore = "360h"
        , secretName = mtls.gitlab.secretName
        }
    
    , observer = {
        name = "polar-gitlab-observer"
        , image = "localhost/polar-gitlab-observer:${chart.appVersion}"
        , gitlabEndpoint = "https://gitlab.sandbox.labz.s-box.org/api/graphql"
        , gitalbSecret = { key = "token" , name = Some "gitlab-secret" }
    }
    , consumer = {
        name = "polar-gitlab-consumer"
        , image = "localhost/polar-gitlab-consumer:${chart.appVersion}"
        , graph = {
             graphDB = "neo4j"
          ,  graphUsername = "neo4j"
          ,  graphPassword = 
              kubernetes.SecretKeySelector::{
                name = Some "polar-graph-pw"
                , key = "secret"
              }
        }
    }
    }



let neo4jPorts = {https = 7473, bolt = 7687 }

-- TODO: Neo4j has various configurations we can add to our own values here
-- Expand and add parameters as desired.

let neo4j =
      { name = "polar-neo4j"
      , hostName = "graph-db.sandbox.labz.s-box.org"
      , namespace = "polar-graph-db"
      , image = "docker.io/library/neo4j:5.10.0-community"
      , podAnnotations = [ RejectSidecarAnnotation ]
      , config = { name = "neo4j-config" , path = "/var/lib/neo4j/neo4j.conf" }
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
      , gateway = { name = "polar-gb-gateway"}
      , logging = { serverLogsXml = "", userLogsXml = "" }
      , resources = { cpu = "1000m", memory = "2Gi" }
      , containerSecurityContext =
          kubernetes.SecurityContext::{
          , runAsGroup = Some 7474
          , runAsNonRoot = Some True
          , runAsUser = Some 7474
          }
      , volumes =
          { data =
              { name = "polar-db-data"
              , storageClassName = Some "standard"
              , storageSize = "100Gi"
              , mountPath = "/var/lib/neo4j/data"
              }
          , logs =
              { name = "polar-db-logs"
              , storageClassName = Some "standard"
              , storageSize = "10Gi"
              , mountPath = "/var/lib/neo4j/logs"
              }
          }
      }

let neo4jBoltAddr = "${neo4j.service.name}.${neo4j.namespace}.svc.cluster.local:${Natural/show neo4jPorts.bolt}"
let neo4jUiAddr = "${neo4j.service.name}.${neo4j.namespace}.svc.cluster.local:${Natural/show neo4jPorts.https}"

in

{   namespace
,   mtls
,   imagePullSecrets
,   cassini
,   cassiniAddr
,   neo4jPorts
,   neo4j
,   neo4jUiAddr
,   neo4jBoltAddr
,   gitlab
}
