-- ACE Tool
--
-- Copyright 2026 Carnegie Mellon University.
--
-- NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
-- FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
-- EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
-- PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
-- MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
-- TRADEMARK, OR COPYRIGHT INFRINGEMENT.
--
-- Licensed under a MIT (SEI)-style license, please see license.txt or contact
-- permission@sei.cmu.edu for full terms.
--
-- [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
-- distribution.  Please see Copyright notice for non-US Government use and distribution.
--
-- This Software includes and/or makes use of Third-Party Software each subject to its own license.
--
-- DM26-0670

let kubernetes =
      ./kubernetes.dhall
        sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let Constants =
      ./constants.dhall

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

-- No extra environment for polar-init. A named value, so the callers that
-- have nothing to add don't each spell out a typed empty list.
let noEnv = [] : List kubernetes.EnvVar.Type

-- What polar-init is told when its certificates come from the credentials agent (through its observer) and
-- not from cert-issuer. The pod is not told where cert-issuer is, and pairs this with an empty
-- certIssuerUrl. For a workload that is not a bootstrap identity (cert-issuer will not sign it on its token
-- alone once the floor is enforced; docs/architecture/cert-issuer-isolation.md). It verifies what it is issued
-- against cert-issuer's CA, so the pod mounts Constants.polarCaVolume and polar-init Constants.polarCaMount, and
-- seals its token only to a session offer signed by the credentials agent, `credentialsAgentIdentity`
-- (docs/architecture/core-restarts.md, "1c in detail"), which render.nu has from the catalog.
let credentialsAgentIssuerEnv =
      \(observerUrl : Text) ->
      \(credentialsAgentIdentity : Text) ->
        [ kubernetes.EnvVar::{ name = "POLAR_INIT_ISSUER", value = Some "credentials-agent" }
        , kubernetes.EnvVar::{ name = "POLAR_INIT_OBSERVER_URL", value = Some observerUrl }
        , kubernetes.EnvVar::{
          , name = "POLAR_INIT_CA_CERT"
          , value = Some "${Constants.polarCaPath}/ca.pem"
          }
        , kubernetes.EnvVar::{
          , name = "POLAR_INIT_CREDENTIALS_AGENT_IDENTITY"
          , value = Some credentialsAgentIdentity
          }
        ]

-- The escrow bootstrap runs only when `extraEnv` asks for it (POLAR_INIT_ESCROW_BOOTSTRAP): the
-- catalog's `encryption` capability does, for an agent that needs encryption, and no hand chart does.
let makePolarInitContainer =
      \(polarInitImage : Text) ->
      \(imagePullPolicy : Text) ->
      \(saTokenVolName : Text) ->
      \(certIssuerUrl : Text) ->
      \(extraEnv : List kubernetes.EnvVar.Type) ->
      \(tokenPath : Text) ->
      \(extraMounts : List kubernetes.VolumeMount.Type) ->
      \(depCerts : List Text) ->
      \(certs : List Text) ->
        kubernetes.Container::{
        , name            = "polar-init"
        , image           = Some polarInitImage
        , imagePullPolicy = Some imagePullPolicy
        , command         = Some [ "polar-init" ]
        , securityContext = Some Constants.DropAllCapSecurityContext
        , env = Some
          (   [ kubernetes.EnvVar::{ name = "POLAR_INIT_CERT_ISSUER_URL", value = Some certIssuerUrl }
              , kubernetes.EnvVar::{ name = "POLAR_INIT_TOKEN_PATH",      value = Some tokenPath      }
              -- polar-crypto's escrow bootstrap (Phase 4) presents this
              -- as its Kubernetes credential. Without it, it falls back
              -- to the automounted token, whose audience is the API
              -- server's, not cert-issuer's.
              , kubernetes.EnvVar::{ name = "KUBERNETES_SA_TOKEN",        value = Some tokenPath      }
              ]
            # Constants.commonClientEnv
            # extraEnv
          )
        , args = Some
          (   Prelude.List.concatMap Text Text (\(d : Text) -> ["--check-dep-cert", d]) depCerts
            # Prelude.List.concatMap Text Text (\(c : Text) -> ["--cert", c]) certs
          )
        , volumeMounts = Some
          (   [ kubernetes.VolumeMount::{ name = saTokenVolName, mountPath = "/workspace" } ]
            # extraMounts
          )
        }

in  { makeGraphEnv
    , makeOpaqueSecret
    , makeDeployment
    , DockerRegistrySecret
    , mkProxySecret
    , ProxyVolume
    , ProxyMount
    , ProxyEnv
    , GraphProxyEnv
    , makePolarInitContainer
    , noEnv
    , credentialsAgentIssuerEnv
    }
