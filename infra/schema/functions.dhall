let kubernetes =
      ./kubernetes.dhall
        sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let Constants =
      ./constants.dhall
        sha256:83837b5e87d39846c41a7ae549d5c65b2c2c8a058c6841b28ed1b746f4c976f2

let Agents =
      ./agents.dhall
        sha256:8458543ac92aa19c950efdf97f5d89c85fbf3be94daad98d716c3217deddd570

let GraphConfig = Agents.GraphConfig

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

let makeGraphEnv
    : Text →
      GraphConfig →
      kubernetes.SecretKeySelector.Type →
      Optional Text →
        List kubernetes.EnvVar.Type
    = λ(addr : Text) →
      λ(g : GraphConfig) →
      λ(passwordSecret : kubernetes.SecretKeySelector.Type) →
      λ(caCertPath : Optional Text) →
          [ kubernetes.EnvVar::{ name = "GRAPH_ENDPOINT", value = Some addr }
          , kubernetes.EnvVar::{ name = "GRAPH_DB", value = Some g.graphDB }
          , kubernetes.EnvVar::{
            , name = "GRAPH_USER"
            , value = Some g.graphUsername
            }
          , kubernetes.EnvVar::{
            , name = "GRAPH_PASSWORD"
            , valueFrom = Some kubernetes.EnvVarSource::{
              , secretKeyRef = Some passwordSecret
              }
            }
          , kubernetes.EnvVar::{
            , name = "GRAPH_CLIENT_CERT"
            , value = Some "/etc/neo4j-client-tls/cert.pem"
            }
          , kubernetes.EnvVar::{
            , name = "GRAPH_CLIENT_KEY"
            , value = Some "/etc/neo4j-client-tls/key.pem"
            }
          ]
        # merge
            { Some =
                λ(path : Text) →
                  [ kubernetes.EnvVar::{
                    , name = "GRAPH_CA_CERT"
                    , value = Some path
                    }
                  ]
            , None = [] : List kubernetes.EnvVar.Type
            }
            caCertPath

let makeOpaqueSecret
    : Text → Text → Text → kubernetes.Secret.Type
    = λ(name : Text) →
      λ(key : Text) →
      λ(value : Text) →
        kubernetes.Secret::{
        , apiVersion = "v1"
        , kind = "Secret"
        , metadata = kubernetes.ObjectMeta::{
          , name = Some name
          , namespace = Some Constants.PolarNamespace
          }
        , stringData = Some [ { mapKey = key, mapValue = value } ]
        , type = Some "Opaque"
        }

let makeDeployment
    : ∀(name : Text) → kubernetes.PodSpec.Type → kubernetes.Deployment.Type
    = λ(name : Text) →
      λ(podSpec : kubernetes.PodSpec.Type) →
        kubernetes.Deployment::{
        , metadata = kubernetes.ObjectMeta::{
          , name = Some name
          , namespace = Some Constants.PolarNamespace
          }
        , spec = Some kubernetes.DeploymentSpec::{
          , selector = kubernetes.LabelSelector::{
            , matchLabels = Some (toMap { name })
            }
          , replicas = Some 1
          , template = kubernetes.PodTemplateSpec::{
            , metadata = Some kubernetes.ObjectMeta::{
              , name = Some name
              , labels = Some [ { mapKey = "name", mapValue = name } ]
              }
            , spec = Some podSpec
            }
          }
        }

let ProxyVolume =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(certName : Text) →
                [ kubernetes.Volume::{
                  , name = certName
                  , secret = Some kubernetes.SecretVolumeSource::{
                    , secretName = Some certName
                    }
                  }
                ]
          , None = [] : List kubernetes.Volume.Type
          }
          cert

let ProxyMount =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(cert : Text) →
                [ kubernetes.VolumeMount::{
                  , name = cert
                  , mountPath = "/etc/tls/proxy"
                  , readOnly = Some True
                  }
                ]
          , None = [] : List kubernetes.VolumeMount.Type
          }
          cert

let ProxyEnv =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(_ : Text) →
                [ kubernetes.EnvVar::{
                  , name = "PROXY_CA_CERT"
                  , value = Some "/etc/tls/proxy/proxy.crt"
                  }
                ]
          , None = [] : List kubernetes.EnvVar.Type
          }
          cert

let GraphProxyEnv =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(_ : Text) →
                [ kubernetes.EnvVar::{
                  , name = "GRAPH_CA_CERT"
                  , value = Some "/etc/tls/proxy/proxy.crt"
                  }
                ]
          , None = [] : List kubernetes.EnvVar.Type
          }
          cert

let mkProxySecret =
      λ(b64 : Text) →
      λ(name : Text) →
      λ(namespace : Text) →
        { apiVersion = "v1"
        , kind = "Secret"
        , metadata = { creationTimestamp = None Text, name, namespace }
        , data.`proxy.pem` = b64
        }

let DockerRegistrySecret =
      λ(secretName : Text) →
      λ(namespace : Text) →
      λ(dockerconfig_b64 : Text) →
        kubernetes.Secret::{
        , apiVersion = "v1"
        , kind = "Secret"
        , metadata = kubernetes.ObjectMeta::{
          , name = Some secretName
          , namespace = Some namespace
          }
        , type = Some "kubernetes.io/dockerconfigjson"
        , data = Some
          [ { mapKey = ".dockerconfigjson", mapValue = dockerconfig_b64 } ]
        }

let makeCertInitContainer =
      \(containerName : Text) ->
      \(certIssuerUrl : Text) ->
      \(certClientImage : Text) ->
      \(certType : Text) ->
      \(certDir : Text) ->
      \(volumeName : Text) ->
      \(saTokenVolName : Text) ->
      \(keyAlgorithm : Text) ->
      \(extraSans : List Text) ->
        kubernetes.Container::{
        , name  = containerName
        , image = Some certClientImage
        , imagePullPolicy = Some "IfNotPresent"
        , args  = Some
          (   [ "--cert-issuer-url", certIssuerUrl
              , "--token-path",      "/workspace/token"
              , "--cert-dir",        certDir
              , "--cert-type",       certType
              , "--key-algorithm",   keyAlgorithm
              ]
            # Prelude.List.concatMap Text Text (\(san : Text) -> ["--extra-san", san]) extraSans
          )
        , securityContext = Some Constants.DropAllCapSecurityContext
        , volumeMounts = Some
          [ kubernetes.VolumeMount::{ name = volumeName,     mountPath = certDir      }
          , kubernetes.VolumeMount::{ name = saTokenVolName, mountPath = "/workspace" }
          ]
        }

let makeCertClientInitContainer =
      \(certIssuerUrl : Text) ->
      \(certClientImage : Text) ->
      \(audience : Text) ->
        makeCertInitContainer
          "cert-client"
          certIssuerUrl certClientImage
          "client" "/etc/tls/certs"
          Constants.certVolumeName Constants.saTokenVolumeName
          "ecdsa-p256"
          ([] : List Text)

let makeCertServerInitContainer =
      \(certIssuerUrl : Text) ->
      \(certClientImage : Text) ->
      \(extraSans : List Text) ->
        makeCertInitContainer
          "cert-client-server"
          certIssuerUrl certClientImage
          "server" "/etc/neo4j-tls"
          "neo4j-tls" "neo4j-sa-token"
          "ecdsa-p256"
          extraSans

in  { makeGraphEnv
    , makeOpaqueSecret
    , makeDeployment
    , DockerRegistrySecret
    , mkProxySecret
    , ProxyVolume
    , ProxyMount
    , ProxyEnv
    , GraphProxyEnv
    , makeCertClientInitContainer
    , makeCertServerInitContainer
    , makeCertInitContainer
    }
