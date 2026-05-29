-- local-oci-registry.dhall
--
-- Deploys a plain Docker registry (no TLS) into the local-registry namespace
-- for Flux testing. Exposes it as a NodePort on 31500 so the host can push
-- to it directly, and as a ClusterIP so pods (including Flux controllers)
-- can pull from it at registry.local-registry.svc.cluster.local:5000.
--
-- Generate YAML:
--   dhall-to-yaml --documents <<< ./local-oci-registry.dhall
--
-- OrbStack note: you must tell Docker that this registry is insecure before
-- pushing. Add the following to ~/.docker/daemon.json (OrbStack reads it):
--
--   {
--     "insecure-registries": ["localhost:31500"]
--   }
--
-- Then restart OrbStack. Push with:
--   docker tag <image> localhost:31500/<name>:<tag>
--   docker push localhost:31500/<name>:<tag>
--
-- From inside the cluster, reference images as:
--   registry.local-registry.svc.cluster.local:5000/<name>:<tag>
--
-- For flux push artifact, use the NodePort address from the host:
--   flux push artifact oci://localhost:31500/my-app:latest \
--     --path=./manifests \
--     --source=https://github.com/my-org/my-repo \
--     --revision=main@sha1:$(git rev-parse HEAD)

let kubernetes = ../../types/kubernetes.dhall
let namespace = "local-registry"

let labels = toMap { app = "registry" }

let ns =
      kubernetes.Namespace::{
      , apiVersion = "v1"
      , kind = "Namespace"
      , metadata = kubernetes.ObjectMeta::{
        , name = Some namespace
        }
      }

-- ── Deployment ───────────────────────────────────────────────────────────────
--
-- Single replica is fine for local testing. No PersistentVolume — images are
-- lost when the pod restarts, which is acceptable; you'll be pushing fresh
-- artifacts each test run anyway. If you want persistence, add a PVC and mount
-- it at /var/lib/registry.

let deployment =
      kubernetes.Deployment::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "registry"
        , namespace = Some namespace
        , labels = Some labels
        }
      , spec = Some kubernetes.DeploymentSpec::{
        , selector = kubernetes.LabelSelector::{
          , matchLabels = Some labels
          }
        , replicas = Some 1
        , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
            , labels = Some labels
            }
          , spec = Some kubernetes.PodSpec::{
            , containers =
              [ kubernetes.Container::{
                , name = "registry"
                , image = Some "registry:2"
                , ports = Some
                  [ kubernetes.ContainerPort::{
                    , containerPort = 5000
                    , name = Some "registry"
                    }
                  ]
                , env = Some
                  [ -- Disable auth entirely — local testing only
                    kubernetes.EnvVar::{
                    , name = "REGISTRY_AUTH"
                    , value = Some ""
                    }
                  , -- Allow deletion so you can overwrite tags freely
                    kubernetes.EnvVar::{
                    , name = "REGISTRY_STORAGE_DELETE_ENABLED"
                    , value = Some "true"
                    }
                  ]
                , readinessProbe = Some kubernetes.Probe::{
                  , httpGet = Some kubernetes.HTTPGetAction::{
                    , path = Some "/v2/"
                    , port = kubernetes.NatOrString.Nat 5000
                    }
                  , initialDelaySeconds = Some 2
                  , periodSeconds = Some 5
                  }
                }
              ]
            }
          }
        }
      }

-- ── Service ──────────────────────────────────────────────────────────────────
--
-- ClusterIP port 5000: used by Flux controllers inside the cluster.
-- NodePort 31500:      used by your host to push images.
--
-- 31500 is arbitrary — pick any port in the NodePort range (30000-32767)
-- that isn't already claimed on your local cluster.

let service =
      kubernetes.Service::{
      , metadata = kubernetes.ObjectMeta::{
        , name = Some "registry"
        , namespace = Some namespace
        , labels = Some labels
        }
      , spec = Some kubernetes.ServiceSpec::{
        , selector = Some labels
        , type = Some "NodePort"
        , ports = Some
          [ kubernetes.ServicePort::{
            , port = 5000
            , targetPort = Some (kubernetes.NatOrString.Nat 5000)
            , nodePort = Some 31500
            , name = Some "registry"
            }
          ]
        }
      }

in  [ kubernetes.Resource.Namespace ns, kubernetes.Resource.Deployment deployment, kubernetes.Resource.Service service ]
