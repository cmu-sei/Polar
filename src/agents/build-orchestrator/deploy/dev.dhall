-- infra/dev.dhall
--
-- Local development infrastructure for Cyclops.
-- Two independent services: MinIO for object storage, distribution for OCI artifacts.
-- They are intentionally decoupled — the registry uses local filesystem storage.
--
-- Generate:
--   dhall-to-yaml-ng --file infra/dev.dhall --output infra/dev-generated.yaml
--
-- Apply:
--   kubectl apply -f infra/dev-generated.yaml
--
-- Access (from Colima host):
--   MinIO S3 API:  http://$(colima ip):30900
--   MinIO console: http://$(colima ip):30901
--   Registry:      http://$(colima ip):30500
--
-- Configure Docker to allow the insecure registry:
--   Add to /etc/docker/daemon.json (or Docker Desktop settings):
--   { "insecure-registries": ["<colima-ip>:30500"] }
let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
        sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let k = kubernetes

let mkLabels
    : Text -> List { mapKey : Text, mapValue : Text }
    = \(app : Text) ->
        [ { mapKey = "app.kubernetes.io/name", mapValue = app }
        , { mapKey = "app.kubernetes.io/managed-by", mapValue = "cyclops-dev" }
        ]

let mkEnv
    : Text -> Text -> k.EnvVar.Type
    = \(name : Text) ->
      \(value : Text) ->
        k.EnvVar::{ name, value = Some value }

let mkSecretEnv
    : Text -> Text -> Text -> k.EnvVar.Type
    = \(envName : Text) ->
      \(secretName : Text) ->
      \(secretKey : Text) ->
        k.EnvVar::{
        , name = envName
        , valueFrom = Some k.EnvVarSource::{
          , secretKeyRef = Some k.SecretKeySelector::{
            , name = Some secretName
            , key = secretKey
            }
          }
        }

let mkPort
    : Natural -> Text -> k.ContainerPort.Type
    = \(port : Natural) ->
      \(name : Text) ->
        k.ContainerPort::{ containerPort = port, name = Some name }

let mkNodePort
    : Natural -> Natural -> Text -> k.ServicePort.Type
    = \(port : Natural) ->
      \(nodePort : Natural) ->
      \(name : Text) ->
        k.ServicePort::{
        , port
        , targetPort = Some (k.NatOrString.Nat port)
        , nodePort = Some nodePort
        , name = Some name
        }

let namespace = "cyclops-dev"

let minioImage = "minio/minio:RELEASE.2025-02-28T09-55-16Z"

let registryImage = "registry:2.8.3"

let namespaceManifest =
      k.Namespace::{
      , metadata = k.ObjectMeta::{
        , name = Some namespace
        , labels = Some (mkLabels "cyclops-dev")
        }
      }

let minioLabels = mkLabels "minio"

let minioSecret =
      k.Secret::{
      , metadata = k.ObjectMeta::{
        , name = Some "minio-credentials"
        , namespace = Some namespace
        , labels = Some minioLabels
        }
      , type = Some "Opaque"
      , stringData = Some
        [ { mapKey = "root-user", mapValue = "minio" }
        , { mapKey = "root-password", mapValue = "minio123" }
        ]
      }

let minioPVC =
      k.PersistentVolumeClaim::{
      , metadata = k.ObjectMeta::{
        , name = Some "minio-data"
        , namespace = Some namespace
        , labels = Some minioLabels
        }
      , spec = Some k.PersistentVolumeClaimSpec::{
        , accessModes = Some [ "ReadWriteOnce" ]
        , resources = Some k.VolumeResourceRequirements::{
          , requests = Some [ { mapKey = "storage", mapValue = "10Gi" } ]
          }
        }
      }

let minioDeployment =
      k.Deployment::{
      , metadata = k.ObjectMeta::{
        , name = Some "minio"
        , namespace = Some namespace
        , labels = Some minioLabels
        }
      , spec = Some k.DeploymentSpec::{
        , replicas = Some 1
        , selector = k.LabelSelector::{ matchLabels = Some minioLabels }
        , template = k.PodTemplateSpec::{
          , metadata = Some k.ObjectMeta::{ labels = Some minioLabels }
          , spec = Some k.PodSpec::{
            , containers =
              [ k.Container::{
                , name = "minio"
                , image = Some minioImage
                , command = Some
                  [ "minio", "server", "/data", "--console-address", ":9001" ]
                , env = Some
                  [ mkSecretEnv
                      "MINIO_ROOT_USER"
                      "minio-credentials"
                      "root-user"
                  , mkSecretEnv
                      "MINIO_ROOT_PASSWORD"
                      "minio-credentials"
                      "root-password"
                  ]
                , ports = Some [ mkPort 9000 "s3-api", mkPort 9001 "console" ]
                , volumeMounts = Some
                  [ k.VolumeMount::{ name = "data", mountPath = "/data" } ]
                , readinessProbe = Some k.Probe::{
                  , httpGet = Some k.HTTPGetAction::{
                    , path = Some "/minio/health/ready"
                    , port = k.NatOrString.Nat 9000
                    }
                  , initialDelaySeconds = Some 10
                  , periodSeconds = Some 5
                  }
                , livenessProbe = Some k.Probe::{
                  , httpGet = Some k.HTTPGetAction::{
                    , path = Some "/minio/health/live"
                    , port = k.NatOrString.Nat 9000
                    }
                  , initialDelaySeconds = Some 20
                  , periodSeconds = Some 10
                  }
                }
              ]
            , volumes = Some
              [ k.Volume::{
                , name = "data"
                , persistentVolumeClaim = Some k.PersistentVolumeClaimVolumeSource::{
                  , claimName = "minio-data"
                  }
                }
              ]
            }
          }
        }
      }

let minioService =
      k.Service::{
      , metadata = k.ObjectMeta::{
        , name = Some "minio"
        , namespace = Some namespace
        , labels = Some minioLabels
        }
      , spec = Some k.ServiceSpec::{
        , type = Some "NodePort"
        , selector = Some minioLabels
        , ports = Some
          [ mkNodePort 9000 30900 "s3-api", mkNodePort 9001 30901 "console" ]
        }
      }

let registryLabels = mkLabels "registry"

let registryPVC =
      k.PersistentVolumeClaim::{
      , metadata = k.ObjectMeta::{
        , name = Some "registry-data"
        , namespace = Some namespace
        , labels = Some registryLabels
        }
      , spec = Some k.PersistentVolumeClaimSpec::{
        , accessModes = Some [ "ReadWriteOnce" ]
        , resources = Some k.VolumeResourceRequirements::{
          , requests = Some [ { mapKey = "storage", mapValue = "20Gi" } ]
          }
        }
      }

let registryConfigMap =
      k.ConfigMap::{
      , metadata = k.ObjectMeta::{
        , name = Some "registry-config"
        , namespace = Some namespace
        , labels = Some registryLabels
        }
      , data = Some
        [ { mapKey = "config.yml"
          , mapValue =
              ''
              version: 0.1

              log:
                level: info
                formatter: json

              storage:
                filesystem:
                  rootdirectory: /var/lib/registry
                delete:
                  enabled: true
                cache:
                  blobdescriptor: inmemory

              http:
                addr: :5000
                headers:
                  X-Content-Type-Options: [nosniff]

              health:
                storagedriver:
                  enabled: false
              ''
          }
        ]
      }

let registryDeployment =
      k.Deployment::{
      , metadata = k.ObjectMeta::{
        , name = Some "registry"
        , namespace = Some namespace
        , labels = Some registryLabels
        }
      , spec = Some k.DeploymentSpec::{
        , replicas = Some 1
        , selector = k.LabelSelector::{ matchLabels = Some registryLabels }
        , template = k.PodTemplateSpec::{
          , metadata = Some k.ObjectMeta::{ labels = Some registryLabels }
          , spec = Some k.PodSpec::{
            , containers =
              [ k.Container::{
                , name = "registry"
                , image = Some registryImage
                , ports = Some [ mkPort 5000 "registry" ]
                , volumeMounts = Some
                  [ k.VolumeMount::{
                    , name = "config"
                    , mountPath = "/etc/docker/registry"
                    , readOnly = Some True
                    }
                  , k.VolumeMount::{
                    , name = "data"
                    , mountPath = "/var/lib/registry"
                    }
                  ]
                , readinessProbe = Some k.Probe::{
                  , httpGet = Some k.HTTPGetAction::{
                    , path = Some "/v2/"
                    , port = k.NatOrString.Nat 5000
                    }
                  , initialDelaySeconds = Some 5
                  , periodSeconds = Some 10
                  }
                }
              ]
            , volumes = Some
              [ k.Volume::{
                , name = "config"
                , configMap = Some k.ConfigMapVolumeSource::{
                  , name = Some "registry-config"
                  }
                }
              , k.Volume::{
                , name = "data"
                , persistentVolumeClaim = Some k.PersistentVolumeClaimVolumeSource::{
                  , claimName = "registry-data"
                  }
                }
              ]
            }
          }
        }
      }

let registryService =
      k.Service::{
      , metadata = k.ObjectMeta::{
        , name = Some "registry"
        , namespace = Some namespace
        , labels = Some registryLabels
        }
      , spec = Some k.ServiceSpec::{
        , type = Some "NodePort"
        , selector = Some registryLabels
        , ports = Some [ mkNodePort 5000 30500 "registry" ]
        }
      }

in  [ k.Resource.Namespace namespaceManifest
    , k.Resource.Secret minioSecret
    , k.Resource.PersistentVolumeClaim minioPVC
    , k.Resource.Deployment minioDeployment
    , k.Resource.Service minioService
    , k.Resource.PersistentVolumeClaim registryPVC
    , k.Resource.ConfigMap registryConfigMap
    , k.Resource.Deployment registryDeployment
    , k.Resource.Service registryService
    ]
