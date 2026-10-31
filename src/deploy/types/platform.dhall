{-
  types/platform.dhall — the units of the core that are not builder agents: the certificate issuer and
  a graph. Both follow the agent contract without being built on polar-agent-builder, so each is a
  `ServiceAgent` (service.dhall) kept alive by the operator like everything else; there is no Deployment
  here. `core.dhall` assembles them with Cassini, the credentials agent and the rest.

    makeCertIssuer   the issuer: its identity, its CA's claim, its configuration document, its Service,
                     and the PolarAgent that runs it. No polar-init (it is the issuer); the
                     service-account token, because it reads the API server's signing keys with it; and
                     no fsGroup, because its CA key must stay mode 0600 and a pod-level group adds
                     group bits to every file on the volume
    makeNeo4j        a graph with a password, for development: no TLS, no client certificates, so there
                     is no certificate to go stale. Its image starts as root and drops to its own user, so
                     it runs as that user directly with its volume owned by that group, and writes outside
                     its volumes, so its root filesystem is writable: a visible exception

  The issuer's configuration is a typed record rendered to JSON, so a field that is missing or
  misnamed fails when the manifests are rendered, not when the issuer starts. The names are the
  issuer's own (`cert-issuer/server/src/config.rs`); the issuance floor is off, which in its
  configuration is the absence of a `bootstrap` list.

  Nothing here holds a secret. The graph's password is referenced by Secret name.
-}
let kubernetes = ./kubernetes.dhall

let C = ./lib-constants.dhall

let Builder = ./builder.dhall

let Service = ./service.dhall

let Settings = ./settings.dhall

let JSON =
      https://prelude.dhall-lang.org/JSON/package.dhall
        sha256:5f98b7722fd13509ef448b075e02b9ff98312ae7a406cf53ed25012dbc9990ac

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

let name = "cert-issuer"

let port = 8443

let caDir = "/home/polar"

let ownTokenDir = "/workspace"

let ownTokenPath = "${ownTokenDir}/token"

let setting = Settings.setting

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
          -- An agent's escrow certificate, its durable encryption identity: it outlives the pods that asked for
          -- it and is issued once for an identity, so an hour (the server lifetime it used to get) made a secret
          -- unsealable an hour after an agent's first start. The fork's setting; required by cert-issuer.
          , serviceLifetimeSecs : Natural
          -- The Secret that holds the CA, made by the plan (`P.certificateAuthority`) before the issuer starts.
          -- `Some`: the issuer only loads it (the CA is then pinned by every agent, from a ConfigMap of the same
          -- operator step). `None`: the issuer makes its own CA on its volume at first start, which nothing can
          -- pin, so it is not enough for a platform whose pods have the custody daemon.
          , caSecret : Optional Text
          -- The floor: who may be signed what on a token alone, and the topics each bootstrap service's certificate
          -- carries, as the JSON list `CERT_ISSUER_BOOTSTRAP` takes (the fork's catalog writes it from the agents'
          -- scaffolds; see docs/ace-catch-up.md). With none, the floor is open and no certificate it signs carries a
          -- topic, so a broker that enforces topics refuses the service everything.
          , floor : Optional Text
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
        , serviceLifetimeSecs = 31536000
        , caSecret = None Text
        , floor = None Text
        }
      }

let duration =
      λ(secs : Natural) →
        JSON.object (toMap { secs = JSON.natural secs, nanos = JSON.natural 0 })

let strings =
      λ(xs : List Text) →
        JSON.array (Prelude.List.map Text JSON.Type JSON.string xs)

--| The token issuers cert-issuer trusts, as the JSON list its `CERT_ISSUER_ISSUERS` variable takes. (It reads no
--| configuration file now: each setting is a variable it declares.)
let issuersJson
    : CertIssuer.Type → Text
    = λ(v : CertIssuer.Type) →
        JSON.render
          ( JSON.array
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
          )

--| The issuer: ServiceAccount, configuration, claim for its CA, Service, and the PolarAgent that runs it.
let makeCertIssuer
    : CertIssuer.Type → { resources : List kubernetes.Resource, agent : Builder.PolarAgentResource }
    = λ(v : CertIssuer.Type) →
        let metadata =
              λ(objectName : Text) →
                kubernetes.ObjectMeta::{
                , name = Some objectName
                , namespace = Some v.namespace
                }

        in  { resources =
              [ kubernetes.Resource.ServiceAccount
                  kubernetes.ServiceAccount::{
                  , metadata = metadata name
                  -- It reads the API server's keys with its own token.
                  , automountServiceAccountToken = Some True
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
                    , selector = Some [ { mapKey = Builder.agentLabel, mapValue = name } ]
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
              ]
            , agent =
                Service.makeServiceAgent
                  Service.ServiceAgent::{
                  , name
                  , namespace = v.namespace
                  , image = v.image
                  , imagePullPolicy = v.imagePullPolicy
                  , port = Some port
                  , env =
                    [ setting "CERT_ISSUER_ISSUERS" (issuersJson v)
                    , setting "CERT_ISSUER_CA_CERT_PATH" "${caDir}/ca/ca.crt"
                    , setting "CERT_ISSUER_CA_KEY_PATH" "${caDir}/ca/ca.key"
                    , setting
                        "CERT_ISSUER_CA_PROVISIONING"
                        ( merge
                            { Some = λ(_ : Text) → "provisioned", None = "self_bootstrap" }
                            v.caSecret
                        )
                    , setting "CERT_ISSUER_DEFAULT_LIFETIME_SECS" (Natural/show v.defaultLifetimeSecs)
                    , setting "CERT_ISSUER_SERVER_LIFETIME_SECS" (Natural/show v.serverLifetimeSecs)
                    , setting "CERT_ISSUER_USER_LIFETIME_SECS" (Natural/show v.userLifetimeSecs)
                    , setting "CERT_ISSUER_SERVICE_LIFETIME_SECS" (Natural/show v.serviceLifetimeSecs)
                    , setting
                        "CERT_ISSUER_SERVER_IDENTITY"
                        "${name}.${v.namespace}.svc.${v.clusterDomain}"
                    , setting "CERT_ISSUER_INTERNAL_CALLERS" v.internalCaller
                    -- It is signed its own client certificate on its own token, which has the issuer's audience.
                    , setting "CERT_ISSUER_OWN_TOKEN" ownTokenPath
                    -- The record of every certificate it signs, which the credentials agent asks for. On the
                    -- claim: a certificate that cannot be recorded is not returned.
                    , setting "CERT_ISSUER_RECORD_PATH" "${caDir}/issued.jsonl"
                    -- It is a Cassini client now: it signs itself a client certificate on its own token and writes it,
                    -- its key and the CA chain where its client reads them, a directory only it can see.
                    , setting "TLS_CLIENT_CERT" C.certPaths.cert
                    , setting "TLS_CLIENT_KEY" C.certPaths.key
                    , setting "TLS_CA_CERT" C.certPaths.ca
                    , setting "BROKER_ADDR" C.cassiniAddr
                    , setting "CASSINI_SERVER_NAME" C.cassiniServerName
                    -- What it requires of the deployment, and so what it is told the deployment provides.
                    , setting "POLAR_CAPABILITIES" "cassini"
                    , setting "RUST_LOG" "info,cert_issuer=debug"
                    , setting "KUBERNETES_CA_CERT" "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"
                    ]
                    # merge
                        { Some = λ(floor : Text) → [ setting "CERT_ISSUER_BOOTSTRAP" floor ]
                        , None = [] : List kubernetes.EnvVar.Type
                        }
                        v.floor
                  , volumes =
                      [ kubernetes.Volume::{
                        , name = "ca"
                        , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                          , claimName = "${name}-ca"
                          }
                        }
                      , kubernetes.Volume::{
                        , name = "own-tls"
                        , emptyDir = Some kubernetes.EmptyDirVolumeSource::{ medium = Some "Memory" }
                        }
                      , kubernetes.Volume::{
                        , name = "own-token"
                        , projected = Some kubernetes.ProjectedVolumeSource::{
                          , sources = Some
                            [ kubernetes.VolumeProjection::{
                              , serviceAccountToken = Some kubernetes.ServiceAccountTokenProjection::{
                                , audience = Some v.audience
                                , expirationSeconds = Some 3600
                                , path = "token"
                                }
                              }
                            ]
                          }
                        }
                      ]
                    # merge
                        { Some =
                            λ(secret : Text) →
                              [ kubernetes.Volume::{
                                , name = "provisioned-ca"
                                , secret = Some kubernetes.SecretVolumeSource::{
                                  , secretName = Some secret
                                  , items = Some
                                    [ { key = "ca.crt", path = "ca.crt", mode = None Natural }
                                    , { key = "ca.key", path = "ca.key", mode = None Natural }
                                    ]
                                  -- Readable by the pod's group alone: the key is the root of the cluster's trust.
                                  , defaultMode = Some 288
                                  }
                                }
                              ]
                        , None = [] : List kubernetes.Volume.Type
                        }
                        v.caSecret
                  , mounts =
                      [ kubernetes.VolumeMount::{ name = "ca", mountPath = caDir }
                      , kubernetes.VolumeMount::{ name = "own-tls", mountPath = C.certDir }
                      , kubernetes.VolumeMount::{
                        , name = "own-token"
                        , mountPath = ownTokenDir
                        , readOnly = Some True
                        }
                      ]
                    # merge
                        { Some =
                            λ(_ : Text) →
                              [ kubernetes.VolumeMount::{
                                , name = "provisioned-ca"
                                , mountPath = "${caDir}/ca"
                                , readOnly = Some True
                                }
                              ]
                        , None = [] : List kubernetes.VolumeMount.Type
                        }
                        v.caSecret
                  , fsGroup = Some 1000
                  , probe = Some kubernetes.Probe::{
                    , tcpSocket = Some kubernetes.TCPSocketAction::{
                      , port = kubernetes.NatOrString.Nat port
                      }
                    , initialDelaySeconds = Some 20
                    , periodSeconds = Some 10
                    , failureThreshold = Some 3
                    }
                  , serviceAccountToken = True
                  }
            }

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
    : Neo4j.Type → { resources : List kubernetes.Resource, agent : Builder.PolarAgentResource }
    = λ(v : Neo4j.Type) →
        let metadata =
              λ(objectName : Text) →
                kubernetes.ObjectMeta::{
                , name = Some objectName
                , namespace = Some v.namespace
                }

        let envValue =
              λ(n : Text) →
              λ(value : Text) →
                kubernetes.EnvVar::{ name = n, value = Some value }

        in  { resources =
              [ kubernetes.Resource.PersistentVolumeClaim
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
                    , selector = Some [ { mapKey = Builder.agentLabel, mapValue = "polar-neo4j" } ]
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
              ]
            , agent =
                Service.makeServiceAgent
                  Service.ServiceAgent::{
                  , name = "polar-neo4j"
                  , namespace = v.namespace
                  , image = v.image
                  , imagePullPolicy = v.imagePullPolicy
                  , port = Some 7687
                  -- The image's own user, so that its entrypoint does not need to start as root to
                  -- drop to it; the claim is owned by that group.
                  , runAsUser = 7474
                  , fsGroup = Some 7474
                  -- It writes its configuration and run state outside /data and /logs.
                  , writableRoot = True
                  , env =
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
                    , envValue "NEO4J_server_memory_heap_initial__size" "512m"
                    , envValue "NEO4J_server_memory_heap_max__size" "512m"
                    , envValue "NEO4J_server_memory_pagecache_size" "256m"
                    ]
                  , volumes =
                    [ kubernetes.Volume::{
                      , name = "data"
                      , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                        , claimName = "polar-db-data"
                        }
                      }
                    ]
                  , mounts =
                    [ kubernetes.VolumeMount::{ name = "data", mountPath = "/data" } ]
                  , probe = Some kubernetes.Probe::{
                    , tcpSocket = Some kubernetes.TCPSocketAction::{
                      , port = kubernetes.NatOrString.Nat 7687
                      }
                    , initialDelaySeconds = Some 90
                    , periodSeconds = Some 15
                    , failureThreshold = Some 5
                    }
                  }
            }

in  { CertIssuer, makeCertIssuer, issuersJson, Neo4j, makeNeo4j }
