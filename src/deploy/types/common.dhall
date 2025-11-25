-- TODO: a file containing some common data strcutures used throughout deployments of polar.
let kubernetes = ./kubernetes.dhall
let proxyUtils = ./proxy-utils.dhall

let cassiniClientEnvVars = [
  kubernetes.EnvVar::{ name = "TLS_CA_CERT", value = Some values.mtls.caCertPath }
, kubernetes.EnvVar::{ name = "TLS_CLIENT_CERT", value = Some values.mtls.serverCertPath }
, kubernetes.EnvVar::{ name = "TLS_CLIENT_KEY", value = Some values.mtls.serverKeyPath }
, kubernetes.EnvVar::{ name = "BROKER_ADDR", value = Some values.cassiniAddr }
, kubernetes.EnvVar::{ name = "CASSINI_SERVER_NAME", value = Some values.cassiniDNSName }
]

-- try to read from environment, otherwise just ignore it
let GraphProxyCert = env: GRAPH_CA_CERT as Text ? None

let commonNeo4jVars =
    [
    , kubernetes.EnvVar::{
        name = "GRAPH_ENDPOINT"
        , value = Some env:GRAPH_ENDPOINT as Text
        }
    , kubernetes.EnvVar::{
        name = "GRAPH_DB"
        , value = Some env:GRAPH_DB as Text
        }
    , kubernetes.EnvVar::{
        name = "GRAPH_USER"
        , value = Some env:GRAPH_USER as Text
        }
    , kubernetes.EnvVar::{
        name = "GRAPH_PASSWORD"
        , valueFrom = Some kubernetes.EnvVarSource::{
            secretKeyRef = Some kubernetes.SecretKeySelector::{
              , name = Some "polar-graph-pw"
              , key = "secret"
              }
            }
        }
    ]
    # proxyUtils.GraphProxyEnv GraphProxyCert

in { cassiniClientEnvVars, commonNeo4jVars }
