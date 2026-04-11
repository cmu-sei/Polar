-- infra/layers/3-workloads/agents/provenance/resolver.dhall

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
          , proxyCACert     : Optional Text
          }
      ) ->

        let volumes =
              [ kubernetes.Volume::{ name = v.tlsSecretName,            secret = Some kubernetes.SecretVolumeSource::{ secretName = Some v.tlsSecretName } }
              , kubernetes.Volume::{
                , name   = Constants.OciRegistrySecret.name
                , secret = Some kubernetes.SecretVolumeSource::{
                  , secretName = Some Constants.OciRegistrySecret.name
                  , items      = Some [ kubernetes.KeyToPath::{ key = "oci-registry-auth", path = "config.json" } ]
                  }
                }
              ] # functions.ProxyVolume v.proxyCACert

        let env = Constants.commonClientEnv # functions.ProxyEnv v.proxyCACert

        let mounts =
              [ kubernetes.VolumeMount::{ name = v.tlsSecretName,           mountPath = Constants.tlsPath,        readOnly = Some True }
              , kubernetes.VolumeMount::{ name = Constants.OciRegistrySecret.name, mountPath = "/home/polar/.docker/" }
              ] # functions.ProxyMount v.proxyCACert

        in  kubernetes.Deployment::{
            , metadata = kubernetes.ObjectMeta::{ name = Some Constants.RegistryResolverName, namespace = Some Constants.PolarNamespace, annotations = Some [ Constants.RejectSidecarAnnotation ] }
            , spec = Some kubernetes.DeploymentSpec::{
              , selector = kubernetes.LabelSelector::{ matchLabels = Some (toMap { name = Constants.RegistryResolverName }) }
              , replicas = Some 1
              , template = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{ name = Some Constants.RegistryResolverName, labels = Some [ { mapKey = "name", mapValue = Constants.RegistryResolverName } ] }
                , spec = Some kubernetes.PodSpec::{
                  , imagePullSecrets = Some v.imagePullSecrets
                  , volumes          = Some volumes
                  , containers = [ kubernetes.Container::{ name = v.name, image = Some v.image, imagePullPolicy = Some v.imagePullPolicy, securityContext = Some Constants.DropAllCapSecurityContext, env = Some env, volumeMounts = Some mounts } ]
                  }
                }
              }
            }

in render
