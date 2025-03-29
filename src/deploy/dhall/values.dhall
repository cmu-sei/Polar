
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
          ,  graphSecret = 
             {
                key = "token"
                , name = Some "neo4j-secret"
             }
        }
    }
    }
let neo4jPorts = {http = 7474, bolt = 7687 }

let neo4j = {
    name = "polar-neo4j"
,   image = "docker.io/library/neo4j:4.4.42"
,   containerPorts =
    [ 
        kubernetes.ContainerPort::{ containerPort = neo4jPorts.http }
      , kubernetes.ContainerPort::{ containerPort = neo4jPorts.bolt }
    ]
,   service = { name = "neo4j-svc" }
}

let neo4jAddr = "${neo4j.service.name}.${namespace}.svc.cluster.local:${Natural/show neo4jPorts.http}"
in

{   namespace
,   imagePullSecrets
,   cassini
,   cassiniAddr
,   neo4jPorts
,   neo4j
,   neo4jAddr
,   gitlab
}
