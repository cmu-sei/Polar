-- infra/layers/2-services/cert-issuer/deployment.dhall

let k8s       = ../../../schema/kubernetes.dhall
let Constants = ../../../schema/constants.dhall

let render =
      \(v :
          { name            : Text
          , image           : Text
          , imagePullPolicy : Text
          , imagePullSecrets : List { name : Optional Text }
          , port            : Natural
          , caVolumeName    : Text
          , caCertPath      : Text
          , caKeyPath       : Text
          }
      ) ->
        k8s.Deployment::{
        , metadata = k8s.ObjectMeta::{
          , name      = Some v.name
          , namespace = Some Constants.PolarNamespace
          }
        , spec = Some k8s.DeploymentSpec::{
          , selector = k8s.LabelSelector::{ matchLabels = Some (toMap { name = v.name }) }
          , replicas = Some 1
          , template = k8s.PodTemplateSpec::{
            , metadata = Some k8s.ObjectMeta::{
              , name   = Some v.name
              , labels = Some [ { mapKey = "name", mapValue = v.name } ]
              }
            , spec = Some k8s.PodSpec::{
              , imagePullSecrets = Some v.imagePullSecrets
              , volumes = Some
                [ k8s.Volume::{
                  , name                  = v.caVolumeName
                  , persistentVolumeClaim = Some k8s.PersistentVolumeClaimVolumeSource::{ claimName = "${v.name}-ca" }
                  }
                , k8s.Volume::{
                  , name      = "config"
                  , configMap = Some k8s.ConfigMapVolumeSource::{ name = Some "${v.name}-config" }
                  }
                , k8s.Volume::{
                  , name      = "kube-api-ca"
                  , hostPath  = Some k8s.HostPathVolumeSource::{ path = "/var/run/secrets/kubernetes.io/serviceaccount" }
                  }
                ]
              , containers =
                [ k8s.Container::{
                  , name            = v.name
                  , image           = Some v.image
                  , imagePullPolicy = Some v.imagePullPolicy
                  , securityContext = Some Constants.DropAllCapSecurityContext
                  , env = Some
                    [ k8s.EnvVar::{
                      , name  = "CERT_ISSUER_CONFIG"
                      , value = Some "/etc/cert-issuer/cert-issuer.json"
                      }
                    , k8s.EnvVar::{
                      , name  = "RUST_LOG"
                      , value = Some "info,cert_issuer=debug"
                      }
                    , k8s.EnvVar::{
                      , name  = "KUBERNETES_CA_CERT"
                      , value = Some "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"
                      }
                    ]
                  , ports = Some
                    [ k8s.ContainerPort::{ containerPort = v.port, name = Some "http" } ]
                  , volumeMounts = Some
                    [ k8s.VolumeMount::{
                      , name      = v.caVolumeName
                      , mountPath = "/home/polar"
                      }
                    , k8s.VolumeMount::{
                      , name      = "config"
                      , mountPath = "/etc/cert-issuer"
                      , readOnly  = Some True
                      }
                    ]
                  }
                ]
              }
            }
          }
        }

in render
