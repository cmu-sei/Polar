{-
  types/service.dhall — a service that follows the agent contract without being built on
  polar-agent-builder, as a PolarAgent resource.

  The contract is only this: never end on purpose with 0 (an ending asks for a replacement), say why in
  /dev/termination-log if there is a reason, and drain within the shutdown budget on SIGTERM. Anything
  that does it can be kept alive by the operator, one instance and never two: the certificate issuer,
  the graph, an identity provider, a database, a proxy.

  What it does not get is what makes an agent an agent: no `polar-init`, no certificate, no health file,
  no probe unless one is given. It is the operator's hardened pod and the volumes and settings stated
  here, and nothing else. Two exceptions to the hardening exist and are visible where they are used: the
  service-account token (`serviceAccountToken`), and a writable root filesystem (`writableRoot`) for
  third-party software that writes outside its volumes.

  Kubernetes shapes throughout: volumes and mounts are Kubernetes volumes and mounts, so a claim, a
  ConfigMap, a Secret and an emptyDir are all the same to this file.
-}
let kubernetes = ./kubernetes.dhall

let Builder = ./builder.dhall

let ServiceAgent =
      { Type =
          { name : Text
          , namespace : Text
          , image : Text
          , imagePullPolicy : Optional Text
          , command : Optional (List Text)
          , args : Optional (List Text)
          , env : List kubernetes.EnvVar.Type
          , port : Optional Natural
          , volumes : List kubernetes.Volume.Type
          , mounts : List kubernetes.VolumeMount.Type
          , probe : Optional kubernetes.Probe.Type
          , runAsUser : Natural
          -- Off unless a volume must be owned by a group: a pod-level group changes the mode of
          -- every file on the volume, which a key file that must stay 0600 cannot survive.
          , fsGroup : Optional Natural
          , serviceAccount : Optional Text
          , shutdownBudgetSecs : Natural
          , serviceAccountToken : Bool
          , writableRoot : Bool
          }
      , default =
        { imagePullPolicy = None Text
        , command = None (List Text)
        , args = None (List Text)
        , env = [] : List kubernetes.EnvVar.Type
        , port = None Natural
        , volumes = [] : List kubernetes.Volume.Type
        , mounts = [] : List kubernetes.VolumeMount.Type
        , probe = None kubernetes.Probe.Type
        , runAsUser = 1000
        , fsGroup = None Natural
        , serviceAccount = None Text
        , shutdownBudgetSecs = 20
        , serviceAccountToken = False
        , writableRoot = False
        }
      }

let makeServiceAgent
    : ServiceAgent.Type → Builder.PolarAgentResource
    = λ(service : ServiceAgent.Type) →
        { apiVersion = Builder.apiVersion
        , kind = Builder.kind
        , metadata = kubernetes.ObjectMeta::{
          , name = Some service.name
          , namespace = Some service.namespace
          }
        , spec =
          { image = service.image
          , agent = None Text
          , needs = [] : List Text
          , shutdownBudgetSecs = service.shutdownBudgetSecs
          , serviceAccount = service.serviceAccount
          , command = service.command
          , args = service.args
          , pod =
            { runAsUser = service.runAsUser
            , fsGroup = service.fsGroup
            , imagePullPolicy = service.imagePullPolicy
            , initContainers = [] : List kubernetes.Container.Type
            , volumes = service.volumes
            , volumeMounts = service.mounts
            , env = service.env
            , ports =
                merge
                  { Some =
                      λ(p : Natural) →
                        [ kubernetes.ContainerPort::{ containerPort = p } ]
                  , None = [] : List kubernetes.ContainerPort.Type
                  }
                  service.port
            , livenessProbe = service.probe
            , automountServiceAccountToken =
                if    service.serviceAccountToken
                then  Some True
                else  None Bool
            , readOnlyRootFilesystem =
                if service.writableRoot then Some False else None Bool
            }
          }
        }

in  { ServiceAgent, makeServiceAgent }
