-- infra/layers/3-workloads/agents/build/deployment.dhall
--
-- Build agent Deployment: orchestrator + processor in one pod.
-- Orchestrator mounts SA token for cluster API access and cyclops config.
-- Processor mounts cyclops config and has graph access.

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v :
          { name            : Text
          , imagePullPolicy : Text
          , imagePullSecrets : List { name : Optional Text }
          , orchestrator :
            { name               : Text
            , image              : Text
            , serviceAccountName : Text
            , secretName         : Text
            }
          , processor : { name : Text, image : Text }
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

        let saTokenVolume =
              kubernetes.Volume::{
              , name   = v.orchestrator.secretName
              , secret = Some kubernetes.SecretVolumeSource::{
                , secretName = Some v.orchestrator.secretName
                }
              }

        let cyclopsVolume =
              kubernetes.Volume::{
              , name   = "build-orchestrator-config"
              , secret = Some kubernetes.SecretVolumeSource::{
                , secretName = Some "build-orchestrator-config"
                }
              }

        let neo4jCAVolume =
              kubernetes.Volume::{
              , name   = "neo4j-bolt-ca"
              , secret = Some kubernetes.SecretVolumeSource::{
                , secretName = Some "neo4j-bolt-ca"
                }
              }

        let volumes =
              [ tlsVolume, saTokenVolume, cyclopsVolume, neo4jCAVolume ]
              # functions.ProxyVolume v.proxyCACert

        let tlsMount =
              kubernetes.VolumeMount::{
              , name      = v.tls.certificateSpec.secretName
              , mountPath = Constants.tlsPath
              , readOnly  = Some True
              }

        let saTokenMount =
              kubernetes.VolumeMount::{
              , name      = v.orchestrator.secretName
              , mountPath = "/var/run/secrets/kubernetes.io/serviceaccount"
              }

        let cyclopsMount =
              kubernetes.VolumeMount::{
              , name      = "build-orchestrator-config"
              , mountPath = "/etc/cyclops"
              , readOnly  = Some True
              }

        let neo4jCAMount =
              kubernetes.VolumeMount::{
              , name      = "neo4j-bolt-ca"
              , mountPath = "/etc/neo4j-ca"
              , readOnly  = Some True
              }

        let baseMounts = [ tlsMount ] # functions.ProxyMount v.proxyCACert

        let orchestratorEnv =
              Constants.commonClientEnv
              # [ kubernetes.EnvVar::{
                  , name      = "KUBE_TOKEN"
                  , valueFrom = Some kubernetes.EnvVarSource::{
                    , secretKeyRef = Some kubernetes.SecretKeySelector::{
                      , name = Some v.orchestrator.secretName
                      , key  = "token"
                      }
                    }
                  }
                , kubernetes.EnvVar::{
                  , name  = "ORCHESTRATOR_CONFIG_FILE"
                  , value = Some "/etc/cyclops/cyclops.yaml"
                  }
                ]

        let processorEnv =
              Constants.commonClientEnv
              # functions.makeGraphEnv
                  v.neo4jBoltAddr
                  Constants.graphConfig
                  Constants.graphSecretKeySelector
                  (Some "/etc/neo4j-ca/ca.pem")
              # [ kubernetes.EnvVar::{
                  , name  = "ORCHESTRATOR_CONFIG_FILE"
                  , value = Some "/etc/cyclops/cyclops.yaml"
                  }
                ]

        in  kubernetes.Deployment::{
            , metadata = kubernetes.ObjectMeta::{
              , name        = Some v.name
              , namespace   = Some Constants.PolarNamespace
              , annotations = Some [ Constants.RejectSidecarAnnotation ]
              }
            , spec = Some kubernetes.DeploymentSpec::{
              , selector = kubernetes.LabelSelector::{
                , matchLabels = Some (toMap { name = v.name })
                }
              , replicas = Some 1
              , template = kubernetes.PodTemplateSpec::{
                , metadata = Some kubernetes.ObjectMeta::{
                  , name   = Some v.name
                  , labels = Some [ { mapKey = "name", mapValue = v.name } ]
                  }
                , spec = Some kubernetes.PodSpec::{
                  , imagePullSecrets   = Some v.imagePullSecrets
                  , serviceAccountName = Some v.orchestrator.serviceAccountName
                  , volumes            = Some volumes
                  , containers =
                    [ kubernetes.Container::{
                      , name            = v.orchestrator.name
                      , image           = Some v.orchestrator.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some Constants.DropAllCapSecurityContext
                      , env             = Some orchestratorEnv
                      , volumeMounts    = Some (baseMounts # [ saTokenMount, cyclopsMount ])
                      }
                    , kubernetes.Container::{
                      , name            = v.processor.name
                      , image           = Some v.processor.image
                      , imagePullPolicy = Some v.imagePullPolicy
                      , securityContext = Some Constants.DropAllCapSecurityContext
                      , env             = Some processorEnv
                      , volumeMounts    = Some (baseMounts # [ cyclopsMount, neo4jCAMount ])
                      }
                    ]
                  }
                }
              }
            }

in render
