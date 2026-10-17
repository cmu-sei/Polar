-- build-orchestrator/deploy/dev.dhall
--
-- Local development infrastructure for Cyclops.
-- Two independent services: an S3-compatible object store for build logs and
-- manifests, and a distribution registry for OCI artifacts. They are
-- intentionally decoupled: the registry uses local filesystem storage.
--
-- The object store is versitygw (Apache-2.0) on its posix backend: each
-- top-level directory under /data is a bucket, so the bucket the orchestrator
-- writes to is created with a plain mkdir at startup. (This was MinIO; MinIO
-- stopped publishing images in 2025 and the pinned tag no longer pulls.)
--
-- Generate (dev.yaml is generated; don't edit it by hand):
--   dhall-to-yaml --documents --file dev.dhall --output dev.yaml
--
-- Apply the object store and the job namespace (no registry):
--   kubectl apply -f dev.yaml -l 'app.kubernetes.io/name in (cyclops-dev,object-store,cyclops-jobs)'
-- Or everything:
--   kubectl apply -f dev.yaml
--
-- Access from the host (NodePorts; on OrbStack these answer on localhost):
--   Object store (S3 API):  http://localhost:30900
--   Registry:               http://localhost:30500
-- In-cluster:
--   http://object-store.cyclops-dev.svc.cluster.local:9000
--
-- Dev-only credentials (see objectStoreSecret): access key "cyclops-dev",
-- secret key "cyclops-dev-secret", bucket "cyclops-build-artifacts".
--
-- Check it works with the orchestrator's own S3 client:
--   cargo test -p build-orchestrator --test object_store -- --ignored
--
-- Configure Docker to allow the insecure registry:
--   Add to /etc/docker/daemon.json (or Docker Desktop settings):
--   { "insecure-registries": ["<host>:30500"] }
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

-- versitygw v1.9.0, pinned by digest.
let objectStoreImage =
      "versity/versitygw@sha256:eb7f786fcd91e72194e48fcf1ac16302d686bf8e3a71434bdbc067a6efd3fdc7"

-- The bucket Cyclops's orchestrator writes logs and manifests to.
let objectStoreBucket = "cyclops-build-artifacts"

let registryImage = "registry:2.8.3"

let namespaceManifest =
      k.Namespace::{
      , metadata = k.ObjectMeta::{
        , name = Some namespace
        , labels = Some (mkLabels "cyclops-dev")
        }
      }

let objectStoreLabels = mkLabels "object-store"

let objectStoreSecret =
      k.Secret::{
      , metadata = k.ObjectMeta::{
        , name = Some "object-store-credentials"
        , namespace = Some namespace
        , labels = Some objectStoreLabels
        }
      , type = Some "Opaque"
      , stringData = Some
        [ { mapKey = "access-key", mapValue = "cyclops-dev" }
        , { mapKey = "secret-key", mapValue = "cyclops-dev-secret" }
        ]
      }

let objectStorePVC =
      k.PersistentVolumeClaim::{
      , metadata = k.ObjectMeta::{
        , name = Some "object-store-data"
        , namespace = Some namespace
        , labels = Some objectStoreLabels
        }
      , spec = Some k.PersistentVolumeClaimSpec::{
        , accessModes = Some [ "ReadWriteOnce" ]
        , resources = Some k.VolumeResourceRequirements::{
          , requests = Some [ { mapKey = "storage", mapValue = "10Gi" } ]
          }
        }
      }

let objectStoreHealth =
      k.Probe::{
      , httpGet = Some k.HTTPGetAction::{
        , path = Some "/health"
        , port = k.NatOrString.Nat 7070
        }
      , periodSeconds = Some 5
      }

let objectStoreDeployment =
      k.Deployment::{
      , metadata = k.ObjectMeta::{
        , name = Some "object-store"
        , namespace = Some namespace
        , labels = Some objectStoreLabels
        }
      , spec = Some k.DeploymentSpec::{
        , replicas = Some 1
        , selector = k.LabelSelector::{ matchLabels = Some objectStoreLabels }
        , template = k.PodTemplateSpec::{
          , metadata = Some k.ObjectMeta::{ labels = Some objectStoreLabels }
          , spec = Some k.PodSpec::{
            , containers =
              [ k.Container::{
                , name = "versitygw"
                , image = Some objectStoreImage
                -- Create the bucket (a directory, on the posix backend), then
                -- serve /data with a health endpoint for the probes.
                , command = Some
                  [ "/bin/sh"
                  , "-c"
                  ,     "mkdir -p /data/${objectStoreBucket} && "
                    ++  "exec versitygw --health /health posix /data"
                  ]
                , env = Some
                  [ mkSecretEnv
                      "ROOT_ACCESS_KEY"
                      "object-store-credentials"
                      "access-key"
                  , mkSecretEnv
                      "ROOT_SECRET_KEY"
                      "object-store-credentials"
                      "secret-key"
                  ]
                , ports = Some [ mkPort 7070 "s3-api" ]
                , volumeMounts = Some
                  [ k.VolumeMount::{ name = "data", mountPath = "/data" } ]
                , readinessProbe = Some objectStoreHealth
                , livenessProbe = Some
                    (objectStoreHealth // { initialDelaySeconds = Some 10 })
                }
              ]
            , volumes = Some
              [ k.Volume::{
                , name = "data"
                , persistentVolumeClaim = Some k.PersistentVolumeClaimVolumeSource::{
                  , claimName = "object-store-data"
                  }
                }
              ]
            }
          }
        }
      }

-- Service port 9000 (the conventional S3 port) in front of versitygw's 7070.
let objectStoreService =
      k.Service::{
      , metadata = k.ObjectMeta::{
        , name = Some "object-store"
        , namespace = Some namespace
        , labels = Some objectStoreLabels
        }
      , spec = Some k.ServiceSpec::{
        , type = Some "NodePort"
        , selector = Some objectStoreLabels
        , ports = Some
          [ k.ServicePort::{
            , port = 9000
            , targetPort = Some (k.NatOrString.Nat 7070)
            , nodePort = Some 30900
            , name = Some "s3-api"
            }
          ]
        }
      }

-- ── Job namespace ────────────────────────────────────────────────────────────
-- Where the orchestrator creates build and analysis Jobs in dev. The quota and
-- limit range are a hard backstop: however many requests arrive (the analysis
-- scheduler, or a hand-published BuildRequest, which nothing throttles), the
-- cluster never runs more than this. Sized for an 8-core / 16 GiB laptop.
--
-- Not created here, because they hold local material (see the README):
--   client-tls  the Cassini client cert jobs mount
--   proxy-ca    the CA bundle, behind a TLS-inspecting proxy
let jobsNamespace = "cyclops-jobs"

let jobsLabels = mkLabels "cyclops-jobs"

let quantities =
      \(entries : List { mapKey : Text, mapValue : Text }) -> Some entries

let jobsNamespaceManifest =
      k.Namespace::{
      , metadata = k.ObjectMeta::{
        , name = Some jobsNamespace
        , labels = Some jobsLabels
        }
      }

let jobsQuota =
      k.ResourceQuota::{
      , metadata = k.ObjectMeta::{
        , name = Some "jobs-quota"
        , namespace = Some jobsNamespace
        , labels = Some jobsLabels
        }
      , spec = Some k.ResourceQuotaSpec::{
        , hard =
            quantities
              [ -- At most this many job pods at once; beyond it the Job
                -- controller waits for room.
                { mapKey = "pods", mapValue = "4" }
              , { mapKey = "requests.cpu", mapValue = "2" }
              , { mapKey = "limits.cpu", mapValue = "4" }
              , { mapKey = "requests.memory", mapValue = "2Gi" }
              , { mapKey = "limits.memory", mapValue = "6Gi" }
              , { mapKey = "requests.ephemeral-storage", mapValue = "8Gi" }
              , { mapKey = "limits.ephemeral-storage", mapValue = "16Gi" }
                -- Finished Jobs linger for an hour (ttlSecondsAfterFinished).
              , { mapKey = "count/jobs.batch", mapValue = "40" }
              ]
        }
      }

let jobsLimitRange =
      k.LimitRange::{
      , metadata = k.ObjectMeta::{
        , name = Some "jobs-limits"
        , namespace = Some jobsNamespace
        , labels = Some jobsLabels
        }
      , spec = Some k.LimitRangeSpec::{
        , limits =
          [ k.LimitRangeItem::{
            , type = "Container"
              -- Containers that set nothing get these.
            , default =
                quantities
                  [ { mapKey = "cpu", mapValue = "500m" }
                  , { mapKey = "memory", mapValue = "1Gi" }
                  , { mapKey = "ephemeral-storage", mapValue = "2Gi" }
                  ]
            , defaultRequest =
                quantities
                  [ { mapKey = "cpu", mapValue = "100m" }
                  , { mapKey = "memory", mapValue = "128Mi" }
                  , { mapKey = "ephemeral-storage", mapValue = "512Mi" }
                  ]
              -- Deliberately below the orchestrator's CI-sized default
              -- (2 CPU / 4Gi): a dev config must lower kubernetes.resources,
              -- or job pods are rejected loudly instead of swamping the node.
            , max =
                quantities
                  [ { mapKey = "cpu", mapValue = "1" }
                  , { mapKey = "memory", mapValue = "2Gi" }
                  , { mapKey = "ephemeral-storage", mapValue = "4Gi" }
                  ]
            }
          ]
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
    , k.Resource.Secret objectStoreSecret
    , k.Resource.PersistentVolumeClaim objectStorePVC
    , k.Resource.Deployment objectStoreDeployment
    , k.Resource.Service objectStoreService
    , k.Resource.Namespace jobsNamespaceManifest
    , k.Resource.ResourceQuota jobsQuota
    , k.Resource.LimitRange jobsLimitRange
    , k.Resource.PersistentVolumeClaim registryPVC
    , k.Resource.ConfigMap registryConfigMap
    , k.Resource.Deployment registryDeployment
    , k.Resource.Service registryService
    ]
