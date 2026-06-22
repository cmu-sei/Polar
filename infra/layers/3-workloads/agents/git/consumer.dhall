-- infra/layers/3-workloads/agents/git/consumer.dhall

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v :
          { name             : Text
          , image            : Text
          , imagePullPolicy  : Text
          , imagePullSecrets : List { name : Optional Text }
          , certClientImage  : Text
          , certIssuerUrl    : Text
          , saTokenAudience  : Text
          , neo4jBoltAddr    : Text
          , proxyCACert      : Optional Text
          }
      ) ->
        let volumes =
              [ Constants.certEmptyDirVolume
              , Constants.saTokenVolume v.saTokenAudience
              , kubernetes.Volume::{ name = Constants.neo4jClientCertVolumeName, emptyDir = Some kubernetes.EmptyDirVolumeSource::{=} }
              , Constants.saTokenVolume v.saTokenAudience // { name = Constants.neo4jClientSaTokenVolName }
              ] # functions.ProxyVolume v.proxyCACert

        let env =
              Constants.commonClientEnv
              # functions.makeGraphEnv v.neo4jBoltAddr Constants.graphConfig Constants.graphSecretKeySelector (Some "/etc/neo4j-client-tls/ca.pem")
              # functions.ProxyEnv v.proxyCACert

        let mounts =
              [ Constants.certVolumeMount
              , kubernetes.VolumeMount::{ name = Constants.neo4jClientCertVolumeName, mountPath = Constants.neo4jClientCertPath, readOnly = Some True }
              ] # functions.ProxyMount v.proxyCACert

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
                  , initContainers   = Some
                    [ functions.makeCertClientInitContainer v.certIssuerUrl v.certClientImage v.saTokenAudience
                    , functions.makeCertInitContainer
                        "neo4j-cert-client"
                        v.certIssuerUrl
                        v.certClientImage
                        "client"
                        Constants.neo4jClientCertPath
                        Constants.neo4jClientCertVolumeName
                        Constants.neo4jClientSaTokenVolName
                        "ecdsa-p256"
                        ([] : List Text)
                    ]
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
