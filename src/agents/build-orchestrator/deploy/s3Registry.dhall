-- infra/dev.dhall
--
-- Local development infrastructure for Cyclops.
-- Generates Kubernetes manifests for MinIO, the OCI registry, and Gitea.
--
-- Prerequisites:
--   dhall-to-yaml installed (https://github.com/dhall-lang/dhall-haskell)
--   The dhall-kubernetes library pinned below
--
-- Generate all manifests:
--   dhall-to-yaml-ng --file infra/dev.dhall --output infra/dev-generated.yaml
--
-- Apply to your Colima cluster:
--   kubectl apply -f infra/dev-generated.yaml
--
-- The three services are deployed into the `cyclops-dev` namespace and
-- communicate over cluster-internal DNS:
--   MinIO:    minio.cyclops-dev.svc.cluster.local:9000
--   Registry: registry.cyclops-dev.svc.cluster.local:5000
--   Gitea:    gitea.cyclops-dev.svc.cluster.local:3000
--
-- From inside build job pods in cyclops-builds, reference these the same way.
-- NodePort services expose them on your Colima node IP for local browser access.

let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/v6.0.0/package.dhall
        sha256:532e110f424ea8a9f960a13b2ca54779ddcac5d5aa531f86d82f41f8f18d7ef1

let k = kubernetes

-- ── Helpers ───────────────────────────────────────────────────────────────────

let mkLabels
    : Text -> Text -> List { mapKey : Text, mapValue : Text }
    = \(app : Text) ->
      \(component : Text) ->
        [ { mapKey = "app.kubernetes.io/name",      mapValue = app }
        , { mapKey = "app.kubernetes.io/component", mapValue = component }
        , { mapKey = "app.kubernetes.io/managed-by", mapValue = "cyclops-dev" }
        ]

let mkEnv
    : Text -> Text -> k.EnvVar.Type
    = \(name : Text) ->
      \(value : Text) ->
        k.EnvVar::{ name, value = Some value }

let mkPort
    : Natural -> Text -> k.ContainerPort.Type
    = \(port : Natural) ->
      \(name : Text) ->
        k.ContainerPort::{ containerPort = port, name = Some name }

let mkServicePort
    : Natural -> Natural -> Text -> k.ServicePort.Type
    = \(port : Natural) ->
      \(nodePort : Natural) ->
      \(name : Text) ->
        k.ServicePort::{
          , port
          , targetPort = Some (k.IntOrString.Int port)
          , nodePort   = Some nodePort
          , name       = Some name
        }

let namespace = "cyclops-dev"

-- ── Namespace ─────────────────────────────────────────────────────────────────

let namespaceManifest =
      k.Namespace::{
        , metadata = k.ObjectMeta::{
            , name   = Some namespace
            , labels = Some (mkLabels "cyclops-dev" "namespace")
          }
      }

-- ═════════════════════════════════════════════════════════════════════════════
-- MinIO
-- S3-compatible object storage backing the OCI registry.
-- Credentials are dev-only placeholders — never use these in production.
-- ═════════════════════════════════════════════════════════════════════════════

let minioLabels = mkLabels "minio" "storage"

-- Secret holding MinIO root credentials.
-- In production these would come from an external secret manager.
let minioSecret =
      k.Secret::{
        , metadata = k.ObjectMeta::{
            , name      = Some "minio-credentials"
            , namespace = Some namespace
            , labels    = Some minioLabels
          }
        , type       = Some "Opaque"
        , stringData = Some
            [ { mapKey = "root-user",     mapValue = "minio" }
            , { mapKey = "root-password", mapValue = "minio123" }
            ]
      }

let minioPVC =
      k.PersistentVolumeClaim::{
        , metadata = k.ObjectMeta::{
            , name      = Some "minio-data"
            , namespace = Some namespace
            , labels    = Some minioLabels
          }
        , spec = Some k.PersistentVolumeClaimSpec::{
            , accessModes = Some [ "ReadWriteOnce" ]
            , resources   = Some k.VolumeResourceRequirements::{
                , requests = Some
                    [ { mapKey = "storage", mapValue = "10Gi" } ]
              }
          }
      }

let minioDeployment =
      k.Deployment::{
        , metadata = k.ObjectMeta::{
            , name      = Some "minio"
            , namespace = Some namespace
            , labels    = Some minioLabels
          }
        , spec = Some k.DeploymentSpec::{
            , replicas = Some 1
            , selector = k.LabelSelector::{
                , matchLabels = Some minioLabels
              }
            , template = k.PodTemplateSpec::{
                , metadata = Some k.ObjectMeta::{
                    , labels = Some minioLabels
                  }
                , spec = Some k.PodSpec::{
                    , containers =
                        [ k.Container::{
                            , name  = "minio"
                            , image = Some "minio/minio:latest"
                            , command = Some
                                [ "minio"
                                , "server"
                                , "/data"
                                , "--console-address"
                                , ":9001"
                                ]
                            , env = Some
                                [ k.EnvVar::{
                                    , name = "MINIO_ROOT_USER"
                                    , valueFrom = Some k.EnvVarSource::{
                                        , secretKeyRef = Some k.SecretKeySelector::{
                                            , name = "minio-credentials"
                                            , key  = "root-user"
                                          }
                                      }
                                  }
                                , k.EnvVar::{
                                    , name = "MINIO_ROOT_PASSWORD"
                                    , valueFrom = Some k.EnvVarSource::{
                                        , secretKeyRef = Some k.SecretKeySelector::{
                                            , name = "minio-credentials"
                                            , key  = "root-password"
                                          }
                                      }
                                  }
                                ]
                            , ports = Some
                                [ mkPort 9000 "s3-api"
                                , mkPort 9001 "console"
                                ]
                            , volumeMounts = Some
                                [ k.VolumeMount::{
                                    , name      = "data"
                                    , mountPath = "/data"
                                  }
                                ]
                            , readinessProbe = Some k.Probe::{
                                , httpGet = Some k.HTTPGetAction::{
                                    , path = "/minio/health/ready"
                                    , port = k.IntOrString.Int 9000
                                  }
                                , initialDelaySeconds = Some 10
                                , periodSeconds       = Some 5
                              }
                            , livenessProbe = Some k.Probe::{
                                , httpGet = Some k.HTTPGetAction::{
                                    , path = "/minio/health/live"
                                    , port = k.IntOrString.Int 9000
                                  }
                                , initialDelaySeconds = Some 20
                                , periodSeconds       = Some 10
                              }
                          }
                        ]
                    , volumes = Some
                        [ k.Volume::{
                            , name = "data"
                            , persistentVolumeClaim = Some
                                k.PersistentVolumeClaimVolumeSource::{
                                  , claimName = "minio-data"
                                }
                          }
                        ]
                  }
              }
          }
      }

-- NodePort exposes MinIO on the Colima node for local tooling (mc, browser).
-- The S3 API is on 30900, console on 30901.
-- From inside the cluster use minio.cyclops-dev.svc.cluster.local:9000.
let minioService =
      k.Service::{
        , metadata = k.ObjectMeta::{
            , name      = Some "minio"
            , namespace = Some namespace
            , labels    = Some minioLabels
          }
        , spec = Some k.ServiceSpec::{
            , type_     = Some "NodePort"
            , selector  = Some minioLabels
            , ports     = Some
                [ mkServicePort 9000 30900 "s3-api"
                , mkServicePort 9001 30901 "console"
                ]
          }
      }

-- ═════════════════════════════════════════════════════════════════════════════
-- OCI Registry
-- distribution/distribution backed by MinIO S3.
-- Depends on MinIO being ready — initContainer waits for the S3 health endpoint.
-- ═════════════════════════════════════════════════════════════════════════════

let registryLabels = mkLabels "registry" "artifacts"

-- The registry config is mounted as a ConfigMap. Credentials are injected
-- as env vars from the minio-credentials Secret at runtime — they are not
-- baked into the ConfigMap.
let registryConfigMap =
      k.ConfigMap::{
        , metadata = k.ObjectMeta::{
            , name      = Some "registry-config"
            , namespace = Some namespace
            , labels    = Some registryLabels
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
                    s3:
                      accesskey:      ''${REGISTRY_S3_ACCESS_KEY}
                      secretkey:      ''${REGISTRY_S3_SECRET_KEY}
                      region:         us-east-1
                      regionendpoint: http://minio.cyclops-dev.svc.cluster.local:9000
                      bucket:         registry
                      secure:         false
                      v4auth:         true
                      chunksize:      33554432
                      rootdirectory:  /
                    redirect:
                      disable: true
                    cache:
                      blobdescriptor: inmemory

                  http:
                    addr:   :5000
                    secret: ''${REGISTRY_HTTP_SECRET}
                    headers:
                      X-Content-Type-Options: [nosniff]

                  health:
                    storagedriver:
                      enabled:   true
                      interval:  10s
                      threshold: 3
                  ''
              }
            ]
      }

let registrySecret =
      k.Secret::{
        , metadata = k.ObjectMeta::{
            , name      = Some "registry-credentials"
            , namespace = Some namespace
            , labels    = Some registryLabels
          }
        , type       = Some "Opaque"
        , stringData = Some
            [ { mapKey = "http-secret",   mapValue = "dev-replace-with-openssl-rand-hex-32" }
            ]
      }

let registryDeployment =
      k.Deployment::{
        , metadata = k.ObjectMeta::{
            , name      = Some "registry"
            , namespace = Some namespace
            , labels    = Some registryLabels
          }
        , spec = Some k.DeploymentSpec::{
            , replicas = Some 1
            , selector = k.LabelSelector::{
                , matchLabels = Some registryLabels
              }
            , template = k.PodTemplateSpec::{
                , metadata = Some k.ObjectMeta::{
                    , labels = Some registryLabels
                  }
                , spec = Some k.PodSpec::{
                    -- Wait for MinIO to be ready before starting the registry.
                    -- Without this the registry crashes on startup because it
                    -- cannot reach the S3 bucket.
                    , initContainers = Some
                        [ k.Container::{
                            , name  = "wait-for-minio"
                            , image = Some "busybox:latest"
                            , command = Some
                                [ "sh"
                                , "-c"
                                , "until wget -qO- http://minio.cyclops-dev.svc.cluster.local:9000/minio/health/ready; do echo waiting for minio; sleep 2; done"
                                ]
                          }
                        ]
                    , containers =
                        [ k.Container::{
                            , name  = "registry"
                            , image = Some "registry:2"
                            , env   = Some
                                [ k.EnvVar::{
                                    , name = "REGISTRY_S3_ACCESS_KEY"
                                    , valueFrom = Some k.EnvVarSource::{
                                        , secretKeyRef = Some k.SecretKeySelector::{
                                            , name = "minio-credentials"
                                            , key  = "root-user"
                                          }
                                      }
                                  }
                                , k.EnvVar::{
                                    , name = "REGISTRY_S3_SECRET_KEY"
                                    , valueFrom = Some k.EnvVarSource::{
                                        , secretKeyRef = Some k.SecretKeySelector::{
                                            , name = "minio-credentials"
                                            , key  = "root-password"
                                          }
                                      }
                                  }
                                , k.EnvVar::{
                                    , name = "REGISTRY_HTTP_SECRET"
                                    , valueFrom = Some k.EnvVarSource::{
                                        , secretKeyRef = Some k.SecretKeySelector::{
                                            , name = "registry-credentials"
                                            , key  = "http-secret"
                                          }
                                      }
                                  }
                                ]
                            , ports = Some [ mkPort 5000 "registry" ]
                            , volumeMounts = Some
                                [ k.VolumeMount::{
                                    , name      = "config"
                                    , mountPath = "/etc/docker/registry"
                                    , readOnly  = Some True
                                  }
                                ]
                            , readinessProbe = Some k.Probe::{
                                , httpGet = Some k.HTTPGetAction::{
                                    , path = "/v2/"
                                    , port = k.IntOrString.Int 5000
                                  }
                                , initialDelaySeconds = Some 5
                                , periodSeconds       = Some 10
                              }
                          }
                        ]
                    , volumes = Some
                        [ k.Volume::{
                            , name = "config"
                            , configMap = Some k.ConfigMapVolumeSource::{
                                , name = "registry-config"
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
            , name      = Some "registry"
            , namespace = Some namespace
            , labels    = Some registryLabels
          }
        , spec = Some k.ServiceSpec::{
            , type_    = Some "NodePort"
            , selector = Some registryLabels
            , ports    = Some [ mkServicePort 5000 30500 "registry" ]
          }
      }

-- ═════════════════════════════════════════════════════════════════════════════
-- Gitea
-- Lightweight self-hosted git service for local repo hosting.
-- Exposes HTTP on 3000 and SSH on 22.
-- ═════════════════════════════════════════════════════════════════════════════

let giteaLabels = mkLabels "gitea" "source-control"

let giteaPVC =
      k.PersistentVolumeClaim::{
        , metadata = k.ObjectMeta::{
            , name      = Some "gitea-data"
            , namespace = Some namespace
            , labels    = Some giteaLabels
          }
        , spec = Some k.PersistentVolumeClaimSpec::{
            , accessModes = Some [ "ReadWriteOnce" ]
            , resources   = Some k.VolumeResourceRequirements::{
                , requests = Some
                    [ { mapKey = "storage", mapValue = "5Gi" } ]
              }
          }
      }

let giteaDeployment =
      k.Deployment::{
        , metadata = k.ObjectMeta::{
            , name      = Some "gitea"
            , namespace = Some namespace
            , labels    = Some giteaLabels
          }
        , spec = Some k.DeploymentSpec::{
            , replicas = Some 1
            , selector = k.LabelSelector::{
                , matchLabels = Some giteaLabels
              }
            , template = k.PodTemplateSpec::{
                , metadata = Some k.ObjectMeta::{
                    , labels = Some giteaLabels
                  }
                , spec = Some k.PodSpec::{
                    , securityContext = Some k.PodSecurityContext::{
                        , runAsUser  = Some 1000
                        , runAsGroup = Some 1000
                        , fsGroup    = Some 1000
                      }
                    , containers =
                        [ k.Container::{
                            , name  = "gitea"
                            , image = Some "docker.gitea.com/gitea:1.25.5"
                            , env   = Some
                                [ mkEnv "USER_UID" "1000"
                                , mkEnv "USER_GID" "1000"
                                -- Tell Gitea its own public URL so clone URLs
                                -- in the web UI are correct for your Colima IP.
                                -- Replace with your actual Colima node IP.
                                , mkEnv "GITEA__server__ROOT_URL"
                                    "http://localhost:30300"
                                , mkEnv "GITEA__server__HTTP_PORT" "3000"
                                , mkEnv "GITEA__server__SSH_PORT"  "22"
                                -- Use SQLite for local dev — no separate DB needed.
                                , mkEnv "GITEA__database__DB_TYPE" "sqlite3"
                                , mkEnv "GITEA__database__PATH"    "/data/gitea/gitea.db"
                                ]
                            , ports = Some
                                [ mkPort 3000 "http"
                                , mkPort 22   "ssh"
                                ]
                            , volumeMounts = Some
                                [ k.VolumeMount::{
                                    , name      = "data"
                                    , mountPath = "/data"
                                  }
                                ]
                            , readinessProbe = Some k.Probe::{
                                , httpGet = Some k.HTTPGetAction::{
                                    , path = "/"
                                    , port = k.IntOrString.Int 3000
                                  }
                                , initialDelaySeconds = Some 15
                                , periodSeconds       = Some 10
                              }
                          }
                        ]
                    , volumes = Some
                        [ k.Volume::{
                            , name = "data"
                            , persistentVolumeClaim = Some
                                k.PersistentVolumeClaimVolumeSource::{
                                  , claimName = "gitea-data"
                                }
                          }
                        ]
                  }
              }
          }
      }

let giteaService =
      k.Service::{
        , metadata = k.ObjectMeta::{
            , name      = Some "gitea"
            , namespace = Some namespace
            , labels    = Some giteaLabels
          }
        , spec = Some k.ServiceSpec::{
            , type_    = Some "NodePort"
            , selector = Some giteaLabels
            , ports    = Some
                [ mkServicePort 3000 30300 "http"
                , mkServicePort 22   30222 "ssh"
                ]
          }
      }

-- ═════════════════════════════════════════════════════════════════════════════
-- MinIO bucket setup job
--
-- Runs once after MinIO is ready to create the `registry` bucket.
-- Without this the registry will fail to start because the bucket doesn't exist.
-- Uses the mc (MinIO client) image. Runs as a k8s Job — idempotent, safe to re-apply.
-- ═════════════════════════════════════════════════════════════════════════════

let bucketSetupJob =
      k.Job::{
        , metadata = k.ObjectMeta::{
            , name      = Some "minio-bucket-setup"
            , namespace = Some namespace
            , labels    = Some (mkLabels "minio-setup" "setup")
          }
        , spec = Some k.JobSpec::{
            , backoffLimit          = Some 5
            , ttlSecondsAfterFinished = Some 300
            , template = k.PodTemplateSpec::{
                , spec = Some k.PodSpec::{
                    , restartPolicy = Some "OnFailure"
                    , containers =
                        [ k.Container::{
                            , name  = "mc"
                            , image = Some "minio/mc:latest"
                            , command = Some [ "/bin/sh" ]
                            , args  = Some
                                [ "-c"
                                , ''
                                  set -e
                                  until mc alias set local \
                                    http://minio.cyclops-dev.svc.cluster.local:9000 \
                                    "$MINIO_ROOT_USER" "$MINIO_ROOT_PASSWORD" \
                                    --api S3v4; do
                                    echo "waiting for minio..."
                                    sleep 3
                                  done
                                  mc mb --ignore-existing local/registry
                                  echo "bucket ready"
                                ''
                                ]
                            , env = Some
                                [ k.EnvVar::{
                                    , name = "MINIO_ROOT_USER"
                                    , valueFrom = Some k.EnvVarSource::{
                                        , secretKeyRef = Some k.SecretKeySelector::{
                                            , name = "minio-credentials"
                                            , key  = "root-user"
                                          }
                                      }
                                  }
                                , k.EnvVar::{
                                    , name = "MINIO_ROOT_PASSWORD"
                                    , valueFrom = Some k.EnvVarSource::{
                                        , secretKeyRef = Some k.SecretKeySelector::{
                                            , name = "minio-credentials"
                                            , key  = "root-password"
                                          }
                                      }
                                  }
                                ]
                          }
                        ]
                  }
              }
          }
      }

-- ═════════════════════════════════════════════════════════════════════════════
-- Output — all resources as a YAML stream
--
-- dhall-to-yaml-ng renders List types as a YAML document stream (--- separated)
-- which kubectl apply -f accepts directly.
-- ═════════════════════════════════════════════════════════════════════════════

in  [ k.Resource.Namespace       namespaceManifest
    -- MinIO
    , k.Resource.Secret           minioSecret
    , k.Resource.PersistentVolumeClaim minioPVC
    , k.Resource.Deployment       minioDeployment
    , k.Resource.Service          minioService
    -- Registry
    , k.Resource.ConfigMap        registryConfigMap
    , k.Resource.Secret           registrySecret
    , k.Resource.Deployment       registryDeployment
    , k.Resource.Service          registryService
    -- Gitea
    , k.Resource.PersistentVolumeClaim giteaPVC
    , k.Resource.Deployment       giteaDeployment
    , k.Resource.Service          giteaService
    -- Setup
    , k.Resource.Job              bucketSetupJob
    ]
