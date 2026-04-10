-- infra/layers/3-workloads/agents/provenance/deployment.dhall
--
-- Provenance agent Deployments: linker and resolver.
-- Rendered as two separate Deployments — they have different volume needs
-- and the resolver requires the OCI registry secret.
--
-- Both unconditionally reject Istio sidecar injection.
-- Naming corrected from sandbox: linker=ArtifactLinkerName, resolver=RegistryResolverName.

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v :
          { imagePullPolicy  : Text
          , imagePullSecrets : List { name : Optional Text }
          , linker    : { name : Text, image : Text }
          , resolver  : { name : Text, image : Text }
          , tls :
            { certificateRequestName : Text
            , certificateSpec :
              { commonName  : Text
              , dnsNames    : List Text
              , duration    : Text
              , issuerRef   : { kind : Text, name : Text }
              , renewBefore : Text
              , secretName  : Text
              }
            }
          , proxyCACert   : Optional Text
          , neo4jBoltAddr : Text
          }
      ) ->

        let tlsVolume =
              kubernetes.Volume::{
              , name   = v.tls.certificateSpec.secretName
              , secret = Some kubernetes.SecretVolumeSource::{
                , secretName = Some v.tls.certificateSpec.secretName
                }
              }

        let neo4jCAVolume =
              kubernetes.Volume::{
              , name   = "neo4j-bolt-ca"
              , secret = Some kubernetes.SecretVolumeSource::{
                , secretName = Some "neo4j-bolt-ca"
                }
              }

        let ociSecretVolume =
              kubernetes.Volume::{
              , name   = Constants.OciRegistrySecret.name
              , secret = Some kubernetes.SecretVolumeSource::{
                , secretName = Some Constants.OciRegistrySecret.name
                , items      = Some
                  [ kubernetes.KeyToPath::{ key = "oci-registry-auth", path = "config.json" } ]
                }
              }

        let tlsMount =
              kubernetes.VolumeMount::{
              , name      = v.tls.certificateSpec.secretName
              , mountPath = Constants.tlsPath
              , readOnly  = Some True
              }

        let neo4jCAMount =
              kubernetes.VolumeMount::{
              , name      = "neo4j-bolt-ca"
              , mountPath = "/etc/neo4j-ca"
              , readOnly  = Some True
              }

        let ociMount =
              kubernetes.VolumeMount::{
              , name      = Constants.OciRegistrySecret.name
              , mountPath = "/home/polar/.docker/"
              }

        let linkerEnv =
              Constants.commonClientEnv
              # functions.makeGraphEnv
                  v.neo4jBoltAddr
                  Constants.graphConfig
                  Constants.graphSecretKeySelector
                  (Some "/etc/neo4j-ca/ca.pem")

        let resolverEnv =
              Constants.commonClientEnv
              # functions.ProxyEnv v.proxyCACert

        let linkerVolumes  = [ tlsVolume, neo4jCAVolume ] # functions.ProxyVolume v.proxyCACert
        let resolverVolumes = [ tlsVolume, ociSecretVolume ] # functions.ProxyVolume v.proxyCACert

        let linkerDeployment =
              kubernetes.Deployment::{
              , metadata = kubernetes.ObjectMeta::{
                , name        = Some Constants.ArtifactLinkerName
                , namespace   = Some Constants.PolarNamespace
                , annotations = Some [ Constants.RejectSidecarAnnotation ]
                }
              , spec = Some kubernetes.DeploymentSpec::{
                , selector = kubernetes.LabelSelector::{
                  , matchLabels = Some (toMap { name = Constants.ArtifactLinkerName })
                  }
                , replicas = Some 1
                , template = kubernetes.PodTemplateSpec::{
                  , metadata = Some kubernetes.ObjectMeta::{
                    , name   = Some Constants.ArtifactLinkerName
                    , labels = Some [ { mapKey = "name", mapValue = Constants.ArtifactLinkerName } ]
                    }
                  , spec = Some kubernetes.PodSpec::{
                    , imagePullSecrets = Some v.imagePullSecrets
                    , volumes          = Some linkerVolumes
                    , containers =
                      [ kubernetes.Container::{
                        , name            = v.linker.name
                        , image           = Some v.linker.image
                        , imagePullPolicy = Some v.imagePullPolicy
                        , securityContext = Some Constants.DropAllCapSecurityContext
                        , env             = Some linkerEnv
                        , volumeMounts    = Some [ tlsMount, neo4jCAMount ]
                        }
                      ]
                    }
                  }
                }
              }

        let resolverDeployment =
              kubernetes.Deployment::{
              , metadata = kubernetes.ObjectMeta::{
                , name        = Some Constants.RegistryResolverName
                , namespace   = Some Constants.PolarNamespace
                , annotations = Some [ Constants.RejectSidecarAnnotation ]
                }
              , spec = Some kubernetes.DeploymentSpec::{
                , selector = kubernetes.LabelSelector::{
                  , matchLabels = Some (toMap { name = Constants.RegistryResolverName })
                  }
                , replicas = Some 1
                , template = kubernetes.PodTemplateSpec::{
                  , metadata = Some kubernetes.ObjectMeta::{
                    , name   = Some Constants.RegistryResolverName
                    , labels = Some [ { mapKey = "name", mapValue = Constants.RegistryResolverName } ]
                    }
                  , spec = Some kubernetes.PodSpec::{
                    , imagePullSecrets = Some v.imagePullSecrets
                    , volumes          = Some resolverVolumes
                    , containers =
                      [ kubernetes.Container::{
                        , name            = v.resolver.name
                        , image           = Some v.resolver.image
                        , imagePullPolicy = Some v.imagePullPolicy
                        , securityContext = Some Constants.DropAllCapSecurityContext
                        , env             = Some resolverEnv
                        , volumeMounts    = Some ([ tlsMount, ociMount ] # functions.ProxyMount v.proxyCACert)
                        }
                      ]
                    }
                  }
                }
              }

        in  [ kubernetes.Resource.Deployment linkerDeployment
            , kubernetes.Resource.Deployment resolverDeployment
            ]

in render
