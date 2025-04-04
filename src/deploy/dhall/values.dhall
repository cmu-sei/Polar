
let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0
let chart = ./chart.dhall
let namespace = "polar"

let imagePullSecrets = [
    { name = "sandbox-registry" }
]

let cassini = 
  {
    name = "cassini"
  , namespace = "polar"
  , image = "localhost/cassini:${chart.appVersion}"
  , port = 8080
  , service = { name = "cassini-ip-svc", type = "ClusterIP" }
  , volumes = 
    [
    , kubernetes.Volume::{
        , name = "mtls-secrets"
        , secret = Some kubernetes.SecretVolumeSource::{
            secretName = Some "cassini-mtls"
        }
      }
    ]
  }

let cassiniAddr = "${cassini.service.name}.${namespace}.svc.cluster.local:${Natural/show cassini.port}"

let graphSecret = 
      kubernetes.SecretKeySelector::{
        key = "secret"
        , name = Some "neo4j-secret"
      }  
let gitlab = {
    name = "gitlab-agent"
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
let neo4jPorts = {http = 7474, bolt = 7687 }

-- TODO: Neo4j has various configurations we can add to our own values here
-- Expand and add parameters as desired.
let neo4j = {
    name = "polar-neo4j"
,   image = "docker.io/library/neo4j:5.10.0-community"
,   config = { name = "neo4j-config" , path = "/var/lib/neo4j/neo4j.conf" }
,   env =  [
      -- load in a default password from secret
      , kubernetes.EnvVar::{
          name = "NEO4J_AUTH"
          , valueFrom = Some kubernetes.EnvVarSource::{
              secretKeyRef = Some graphSecret
          }
      }           
  ]
,   containerPorts =
    [ 
        kubernetes.ContainerPort::{ containerPort = neo4jPorts.http }
      , kubernetes.ContainerPort::{ containerPort = neo4jPorts.bolt }
    ]
, service = { name = "polar-db-svc" }
, logging = { serverLogsXml = "", userLogsXml = "" }
, resources = { cpu = "1000m", memory = "2Gi" }
, containerSecurityContext =
  kubernetes.SecurityContext::{
  , runAsGroup = Some 7474
  , runAsNonRoot = Some True
  , runAsUser = Some 7474
  }
  
, volumes =
  { backups = {=}
  , data =
    { name = "neo4j-data"
    , defaultStorageClass = { accessModes = [ "ReadWriteOnce" ], requests.storage = "10Gi" }
    , disableSubPathExpr = False
    , dynamic =
      { accessModes = [ "ReadWriteOnce" ]
      , requests.storage = "100Gi"
      , storageClassName = "neo4j"
      }
    , labels = {=}
    , mode = ""
    , mountPath = "/var/lib/neo4j/data"
    , selector =
      { accessModes = [ "ReadWriteOnce" ]
      , requests.storage = "10Gi"
      , selectorTemplate.matchLabels
        =
        { app = "polar-neo4j"
        , 
        }
      , storageClassName = "standard"
      }
    , volume.setOwnerAndGroupWritableFilePermissions = False
    , volumeClaimTemplate = {=}
    }
  , logs =
    {
      name = "neo4j-logs"
      , pvcName = "neo4j-logs-pvc"
      , mountPath = "/var/lib/neo4j/logs"
    }
  }
}

let neo4jBoltAddr = "${neo4j.service.name}.${namespace}.svc.cluster.local:${Natural/show neo4jPorts.bolt}"
let neo4jUiAddr = "${neo4j.service.name}.${namespace}.svc.cluster.local:${Natural/show neo4jPorts.http}"

in

{   namespace
,   imagePullSecrets
,   cassini
,   cassiniAddr
,   neo4jPorts
,   neo4j
,   neo4jUiAddr
,   neo4jBoltAddr
,   gitlab
}
