-- infra/layers/3-workloads/agents/kube/observer.dhall

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v :
          { name               : Text
          , image              : Text
          , imagePullPolicy    : Text
          , imagePullSecrets   : List { name : Optional Text }
          , certClientImage    : Text
          , certIssuerUrl      : Text
          , saTokenAudience    : Text
          , serviceAccountName : Text
          , secretName         : Text
          , proxyCACert        : Optional Text
          }
      ) ->
        let volumes =
              [ Constants.certEmptyDirVolume
              , Constants.saTokenVolume v.saTokenAudience
              , kubernetes.Volume::{ name = v.secretName, secret = Some kubernetes.SecretVolumeSource::{ secretName = Some v.secretName } }
              ] # functions.ProxyVolume v.proxyCACert

        let env =
              Constants.commonClientEnv
              # functions.ProxyEnv v.proxyCACert
              # [ kubernetes.EnvVar::{ name = "KUBE_TOKEN", valueFrom = Some kubernetes.EnvVarSource::{ secretKeyRef = Some kubernetes.SecretKeySelector::{ name = Some v.secretName, key = "token" } } } ]

        let mounts =
              [ Constants.certVolumeMount
              , kubernetes.VolumeMount::{ name = v.secretName, mountPath = "/var/run/secrets/kubernetes.io/serviceaccount" }
              ] # functions.ProxyMount v.proxyCACert

        in  kubernetes.Deployment::{
            , metadata = kubernetes.ObjectMeta::{ name = Some v.name, namespace = Some Constants.PolarNamespace, annotations = Some [ Constants.RejectSidecarAnnotation ] }
            , spec = Some kubernetes.DeploymentSpec::{
              , selector = kubernetes.LabelSelector::{ matchLabels = Some (toMap { name = v.name }) }
              , replicas = Some 1
              , template = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{ name = Some v.name, labels = Some [ { mapKey = "name", mapValue = v.name } ] }
                , spec = Some kubernetes.PodSpec::{
                  , imagePullSecrets   = Some v.imagePullSecrets
                  , serviceAccountName = Some v.serviceAccountName
                  , volumes            = Some volumes
                  , initContainers     = Some [ functions.makeCertClientInitContainer v.certIssuerUrl v.certClientImage v.saTokenAudience ]
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
