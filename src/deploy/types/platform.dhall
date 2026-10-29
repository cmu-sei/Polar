{-
  types/platform.dhall — the platform Polar agents run on, as far as it is not itself an agent: the
  certificate issuer and a graph.

  Cassini, the credentials agent and its observer are agents, and are written with `builder.dhall`
  (see local/fork/). What is here is what they stand on:

    makeCertIssuer   the issuer: its identity, its CA's claim, its configuration document, its Service
                     and a Deployment that never overlaps itself (the CA is on a ReadWriteOnce claim)
    makeNeo4j        a graph with a password, for development: no TLS, no client certificates

  The issuer's configuration is a typed record rendered to JSON, so a field that is missing or
  misnamed fails when the manifests are rendered, not when the issuer starts. The names are the
  issuer's own (`cert-issuer/server/src/config.rs`); the issuance floor is off, which in its
  configuration is the absence of a `bootstrap` list.

  Nothing here holds a secret. The graph's password is referenced by Secret name.
-}
let kubernetes = ./kubernetes.dhall

let C = ./lib-constants.dhall

let F = ./functions.dhall

let JSON =
      https://prelude.dhall-lang.org/JSON/package.dhall
        sha256:5f98b7722fd13509ef448b075e02b9ff98312ae7a406cf53ed25012dbc9990ac

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

let name = "cert-issuer"

let port = 8443

let caDir = "/home/polar"

let configDir = "/etc/cert-issuer"

let configFile = "cert-issuer.json"

--| What a deployer states about the issuer.
let CertIssuer =
      { Type =
          { namespace : Text
          , image : Text
          , imagePullPolicy : Optional Text
          , storageClass : Optional Text
          , caSize : Text
          , clusterDomain : Text
          -- The token issuer the cluster's API server signs service-account tokens as, and where its
          -- keys are. Both must be what the API server says (`/.well-known/openid-configuration`).
          , tokenIssuer : Text
          , jwksUri : Text
          , audience : Text
          -- Identities allowed to ask the issuer to sign on another's behalf (/internal/issue): the
          -- credentials agent, by the name its certificate carries.
          , internalCaller : Text
          , defaultLifetimeSecs : Natural
          , serverLifetimeSecs : Natural
          , userLifetimeSecs : Natural
          }
      , default =
        { imagePullPolicy = None Text
        , storageClass = None Text
        , caSize = "1Gi"
        , clusterDomain = "cluster.local"
        , tokenIssuer = "https://kubernetes.default.svc.cluster.local"
        , jwksUri = "https://kubernetes.default.svc.cluster.local/openid/v1/jwks"
        , audience = C.certIssuerAudience
        , defaultLifetimeSecs = 3600
        , serverLifetimeSecs = 3600
        , userLifetimeSecs = 31536000
        }
      }

let duration =
      λ(secs : Natural) →
        JSON.object (toMap { secs = JSON.natural secs, nanos = JSON.natural 0 })

let strings =
      λ(xs : List Text) →
        JSON.array (Prelude.List.map Text JSON.Type JSON.string xs)

--| The issuer's configuration document, as JSON text.
-- TODO we should use the config issuer's existing dhall configuration if we can to get tighter validation.
let certIssuerConfig
    : CertIssuer.Type → Text
    = λ(v : CertIssuer.Type) →
        JSON.render
          ( JSON.object
              ( toMap
                  { bind_addr = JSON.string "0.0.0.0:${Natural/show port}"
                  , ca =
                      JSON.object
                        ( toMap
                            { ca_cert_path = JSON.string "${caDir}/ca/ca.crt"
                            , ca_key_path = JSON.string "${caDir}/ca/ca.key"
                            -- The issuer makes its own CA, on its claim, the first time it starts.
                            , provisioning = JSON.string "self_bootstrap"
                            , default_lifetime = duration v.defaultLifetimeSecs
                            , server_lifetime = duration v.serverLifetimeSecs
                            , user_lifetime = duration v.userLifetimeSecs
                            , identity_lifetime_overrides = JSON.object ([] : List { mapKey : Text, mapValue : JSON.Type })
                            }
                        )
                  , issuers =
                      JSON.array
                        [ JSON.object
                            ( toMap
                                { issuer = JSON.string v.tokenIssuer
                                , audience = strings [ v.audience ]
                                , jwks_uri = JSON.string v.jwksUri
                                , workload_identity_claim = JSON.string "sub"
                                , instance_binding_claim = JSON.string "kubernetes.io/pod/uid"
                                , identity_normalization =
                                    JSON.object
                                      ( toMap
                                          { strategy = JSON.string "kubernetes_service_account"
                                          , cluster_domain = JSON.string v.clusterDomain
                                          }
                                      )
                                , identity_scope_claim = JSON.null
                                , allowed_algorithms = strings [ "RS256", "ES256", "EdDSA" ]
                                , jwks_cache_ttl_min = duration 30
                                , jwks_cache_ttl_max = duration 3600
                                }
                            )
                        ]
                  , mtls =
                      JSON.object
                        ( toMap
                            { server_identity =
                                JSON.string "${name}.${v.namespace}.svc.${v.clusterDomain}"
                            , internal_callers = strings [ v.internalCaller ]
                            }
                        )
                  }
              )
          )

--| The issuer: ServiceAccount, configuration, claim for its CA, Service and Deployment.
let makeCertIssuer
    : CertIssuer.Type → List kubernetes.Resource
    = λ(v : CertIssuer.Type) →
        let metadata =
              λ(objectName : Text) →
                kubernetes.ObjectMeta::{
                , name = Some objectName
                , namespace = Some v.namespace
                }

        in  [ kubernetes.Resource.ServiceAccount
                kubernetes.ServiceAccount::{
                , metadata = metadata name
                -- It reads the API server's keys with its own token.
                , automountServiceAccountToken = Some True
                }
            , kubernetes.Resource.ConfigMap
                kubernetes.ConfigMap::{
                , metadata = metadata "${name}-config"
                , data = Some
                  [ { mapKey = configFile, mapValue = certIssuerConfig v } ]
                }
            , kubernetes.Resource.PersistentVolumeClaim
                kubernetes.PersistentVolumeClaim::{
                , metadata = metadata "${name}-ca"
                , spec = Some kubernetes.PersistentVolumeClaimSpec::{
                  , accessModes = Some [ "ReadWriteOnce" ]
                  , storageClassName = v.storageClass
                  , resources = Some kubernetes.VolumeResourceRequirements::{
                    , requests = Some [ { mapKey = "storage", mapValue = v.caSize } ]
                    }
                  }
                }
            , kubernetes.Resource.Service
                kubernetes.Service::{
                , metadata = metadata name
                , spec = Some kubernetes.ServiceSpec::{
                  , selector = Some (toMap { name })
                  , type = Some "ClusterIP"
                  , ports = Some
                    [ kubernetes.ServicePort::{
                      , name = Some "https"
                      , port
                      , targetPort = Some (kubernetes.NatOrString.Nat port)
                      }
                    ]
                  }
                }
            , kubernetes.Resource.Deployment
                ( F.makeDeploymentIn
                    v.namespace
                    name
                    kubernetes.PodSpec::{
                    , serviceAccountName = Some name
                    , automountServiceAccountToken = Some True
                    , securityContext = Some kubernetes.PodSecurityContext::{
                      , fsGroup = Some 1000
                      }
                    , volumes = Some
                      [ kubernetes.Volume::{
                        , name = "ca"
                        , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                          , claimName = "${name}-ca"
                          }
                        }
                      , kubernetes.Volume::{
                        , name = "config"
                        , configMap = Some kubernetes.ConfigMapVolumeSource::{
                          , name = Some "${name}-config"
                          }
                        }
                      ]
                    , containers =
                      [ kubernetes.Container::{
                        , name
                        , image = Some v.image
                        , imagePullPolicy = v.imagePullPolicy
                        -- The image names uid 10000; the claim and the chart say 1000.
                        , securityContext = Some kubernetes.SecurityContext::{
                          , runAsUser = Some 1000
                          , runAsGroup = Some 1000
                          , runAsNonRoot = Some True
                          , allowPrivilegeEscalation = Some False
                          , capabilities = Some kubernetes.Capabilities::{
                            , drop = Some [ "ALL" ]
                            }
                          }
                        , env = Some
                          [ kubernetes.EnvVar::{
                            , name = "CERT_ISSUER_CONFIG"
                            , value = Some "${configDir}/${configFile}"
                            }
                          , kubernetes.EnvVar::{
                            , name = "RUST_LOG"
                            , value = Some "info,cert_issuer=debug"
                            }
                          , kubernetes.EnvVar::{
                            , name = "KUBERNETES_CA_CERT"
                            , value = Some "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"
                            }
                          ]
                        , ports = Some
                          [ kubernetes.ContainerPort::{
                            , containerPort = port
                            , name = Some "https"
                            }
                          ]
                        , readinessProbe = Some kubernetes.Probe::{
                          , tcpSocket = Some kubernetes.TCPSocketAction::{
                            , port = kubernetes.NatOrString.Nat port
                            }
                          , periodSeconds = Some 5
                          }
                        , volumeMounts = Some
                          [ kubernetes.VolumeMount::{ name = "ca", mountPath = caDir }
                          , kubernetes.VolumeMount::{
                            , name = "config"
                            , mountPath = configDir
                            , readOnly = Some True
                            }
                          ]
                        }
                      ]
                    }
                )
            ]

--| What a deployer states about the graph.
let Neo4j =
      { Type =
          { namespace : Text
          , image : Text
          , imagePullPolicy : Optional Text
          , storageClass : Optional Text
          , size : Text
          -- The Secret holding `neo4j/<password>` under `authKey`; the agents' own Secret holds the
          -- bare password. Neither is rendered.
          , authSecret : Text
          , authKey : Text
          , serviceName : Text
          }
      , default =
        { imagePullPolicy = None Text
        , storageClass = None Text
        , size = "5Gi"
        , authSecret = "neo4j-secret"
        , authKey = "secret"
        , serviceName = "polar-db-svc"
        }
      }

--| A graph with a password and nothing else, for development. The fork's own sets up TLS and
--| client certificates, which neither Blitz nor the credentials agent's graph section needs.
let makeNeo4j
    : Neo4j.Type → List kubernetes.Resource
    = λ(v : Neo4j.Type) →
        let metadata =
              λ(objectName : Text) →
                kubernetes.ObjectMeta::{
                , name = Some objectName
                , namespace = Some v.namespace
                }

        in  [ kubernetes.Resource.PersistentVolumeClaim
                kubernetes.PersistentVolumeClaim::{
                , metadata = metadata "polar-db-data"
                , spec = Some kubernetes.PersistentVolumeClaimSpec::{
                  , accessModes = Some [ "ReadWriteOnce" ]
                  , storageClassName = v.storageClass
                  , resources = Some kubernetes.VolumeResourceRequirements::{
                    , requests = Some [ { mapKey = "storage", mapValue = v.size } ]
                    }
                  }
                }
            , kubernetes.Resource.Service
                kubernetes.Service::{
                , metadata = metadata v.serviceName
                , spec = Some kubernetes.ServiceSpec::{
                  , selector = Some (toMap { name = "polar-neo4j" })
                  , type = Some "ClusterIP"
                  , ports = Some
                    [ kubernetes.ServicePort::{
                      , name = Some "bolt"
                      , port = 7687
                      , targetPort = Some (kubernetes.NatOrString.Nat 7687)
                      }
                    ]
                  }
                }
            , kubernetes.Resource.Deployment
                ( F.makeDeploymentIn
                    v.namespace
                    "polar-neo4j"
                    kubernetes.PodSpec::{
                    , volumes = Some
                      [ kubernetes.Volume::{
                        , name = "data"
                        , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                          , claimName = "polar-db-data"
                          }
                        }
                      ]
                    , containers =
                      [ kubernetes.Container::{
                        , name = "neo4j"
                        , image = Some v.image
                        , imagePullPolicy = v.imagePullPolicy
                        , env = Some
                          [ kubernetes.EnvVar::{
                            , name = "NEO4J_AUTH"
                            , valueFrom = Some kubernetes.EnvVarSource::{
                              , secretKeyRef = Some kubernetes.SecretKeySelector::{
                                , name = Some v.authSecret
                                , key = v.authKey
                                }
                              }
                            }
                          -- Small enough for a laptop; the image's defaults are not.
                          , kubernetes.EnvVar::{
                            , name = "NEO4J_server_memory_heap_initial__size"
                            , value = Some "512m"
                            }
                          , kubernetes.EnvVar::{
                            , name = "NEO4J_server_memory_heap_max__size"
                            , value = Some "512m"
                            }
                          , kubernetes.EnvVar::{
                            , name = "NEO4J_server_memory_pagecache_size"
                            , value = Some "256m"
                            }
                          ]
                        , ports = Some
                          [ kubernetes.ContainerPort::{ containerPort = 7687 } ]
                        , volumeMounts = Some
                          [ kubernetes.VolumeMount::{
                            , name = "data"
                            , mountPath = "/data"
                            }
                          ]
                        }
                      ]
                    }
                )
            ]

in  { CertIssuer, makeCertIssuer, certIssuerConfig, Neo4j, makeNeo4j }
