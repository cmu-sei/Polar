-- infra/layers/3-workloads/agents/openapi/processor.dhall
--
-- OpenAPI processor deployment.
-- Reads Cassini topics and writes API spec data to the graph.

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v :
          { name            : Text
          , image           : Text
          , imagePullPolicy : Text
          , imagePullSecrets : List { name : Optional Text }
          , tlsSecretName   : Text
          , neo4jBoltAddr   : Text
          }
      ) ->

        let volumes =
              [ kubernetes.Volume::{ name = v.tlsSecretName, secret = Some kubernetes.SecretVolumeSource::{ secretName = Some v.tlsSecretName } }
              , kubernetes.Volume::{ name = "neo4j-bolt-ca", configMap = Some kubernetes.ConfigMapVolumeSource::{ name = Some "neo4j-bolt-ca" } }
              ]

        let env =
              Constants.commonClientEnv
              # functions.makeGraphEnv v.neo4jBoltAddr Constants.graphConfig Constants.graphSecretKeySelector (Some "/etc/neo4j-ca/ca.pem")

        let mounts =
              [ kubernetes.VolumeMount::{ name = v.tlsSecretName, mountPath = Constants.tlsPath, readOnly = Some True }
              , kubernetes.VolumeMount::{ name = "neo4j-bolt-ca", mountPath = "/etc/neo4j-ca",   readOnly = Some True }
              ]

        in  kubernetes.Deployment::{
            , metadata = kubernetes.ObjectMeta::{ name = Some v.name, namespace = Some Constants.PolarNamespace, annotations = Some [ Constants.RejectSidecarAnnotation ] }
            , spec = Some kubernetes.DeploymentSpec::{
              , selector = kubernetes.LabelSelector::{ matchLabels = Some (toMap { name = v.name }) }
              , replicas = Some 1
              , template = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{ name = Some v.name, labels = Some [ { mapKey = "name", mapValue = v.name } ] }
                , spec = Some kubernetes.PodSpec::{
                  , imagePullSecrets = Some v.imagePullSecrets
                  , volumes          = Some volumes
                  , containers =
                    [ kubernetes.Container::{
                      , name            = v.name
                      , image           = Some v.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some Constants.DropAllCapSecurityContext
                      , env             = Some env
                      , volumeMounts    = Some mounts
                      }
                    ]
                  }
                }
              }
            }

in render
