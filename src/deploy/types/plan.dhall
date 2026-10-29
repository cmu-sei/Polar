{-
  types/plan.dhall — a plan: what to put on a cluster, in what order, and what to wait for.

  This is the spec a person reads and writes. `polar up` runs it; it knows nothing else about what is
  being deployed. Order, environments and secrets are the consumer's, so they are stated here, in the
  consumer's own plan, and not decided by the library or the tool.

    Plan    a name and stages, run in order
    Stage   the CRD if asked for, the namespaces, the secrets, the other objects, then the waits;
            the next stage starts when the waits have passed
    Object  any Kubernetes object, or a PolarAgent, applied server-side so that running it again is
            a no-op
    Secret  made only when it is absent, with values that are never printed. Secrets in one `group`
            are made together, sharing the random values they name, or not at all
    Wait    until a field of an object has a value: a Deployment's readiness, a PolarAgent's phase

  A stage that is `optional` runs only when asked for (`polar up --with <stage>`). A stage that is
  `shared` belongs to every plan that includes it (the core): `polar down` leaves it in place unless
  asked, and plans that share it apply it once. A stage with
  `installCrd` first installs the PolarAgent CustomResourceDefinition, which the tool carries (it is
  generated from the operator's own types, so it cannot disagree with them).

  The rendered form is YAML: `dhall-to-yaml --file plan.dhall > plan.yaml`, or `polar render`. Unions
  render as their contents, so the alternatives of `Value` are told apart by their field names.
-}
let kubernetes = ./kubernetes.dhall

let Builder = ./builder.dhall

let Operator = ./operator.dhall

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

--| Anything a stage can put on the cluster.
let Object = < K8s : kubernetes.Resource | PolarAgent : Builder.PolarAgentResource >

--| Where a secret's key gets its value. Written without a tag: one of these fields says which.
let Value =
      < Literal : { literal : Text }
      | Random : { random : Natural, id : Optional Text }
      | Template : { template : Text }
      | Copy : { copy : { namespace : Text, name : Text, key : Text } }
      >

let Secret =
      { Type =
          { namespace : Text
          , name : Text
          , group : Optional Text
          , keys : List { mapKey : Text, mapValue : Value }
          }
      , default.group = None Text
      }

let Wait =
      { Type =
          { apiVersion : Text
          , kind : Text
          , namespace : Optional Text
          , name : Text
          , field : Text
          , equals : Text
          , timeoutSecs : Natural
          }
      , default = { namespace = None Text, timeoutSecs = 300 }
      }

let Stage =
      { Type =
          { name : Text
          , description : Optional Text
          , optional : Bool
          -- Belongs to more than one plan (the core every application comes with): `polar down` leaves
          -- it alone, and plans that share it apply it once.
          , shared : Bool
          , installCrd : Bool
          , secrets : List Secret.Type
          , objects : List Object
          , waits : List Wait.Type
          }
      , default =
        { description = None Text
        , optional = False
        , shared = False
        , installCrd = False
        , secrets = [] : List Secret.Type
        , objects = [] : List Object
        , waits = [] : List Wait.Type
        }
      }

let Plan = { name : Text, stages : List Stage.Type }

-- Wrapping a list of one kind of thing as objects.
let k8s
    : List kubernetes.Resource → List Object
    = Prelude.List.map kubernetes.Resource Object Object.K8s

let agents
    : List Builder.PolarAgentResource → List Object
    = Prelude.List.map Builder.PolarAgentResource Object Object.PolarAgent

--| Wait until a Deployment has its one replica ready.
let waitForDeployment
    : Text → Text → Wait.Type
    = λ(namespace : Text) →
      λ(name : Text) →
        Wait::{
        , apiVersion = "apps/v1"
        , kind = "Deployment"
        , namespace = Some namespace
        , name
        , field = "status.readyReplicas"
        , equals = "1"
        }

--| Wait until the operator reports a PolarAgent's instance running.
let waitForAgent
    : Text → Text → Wait.Type
    = λ(namespace : Text) →
      λ(name : Text) →
        Wait::{
        , apiVersion = Builder.apiVersion
        , kind = Builder.kind
        , namespace = Some namespace
        , name
        , field = "status.phase"
        , equals = "Running"
        }

--| A secret of one key holding random bytes, as hex.
let randomSecret
    : Text → Text → Text → Natural → Secret.Type
    = λ(namespace : Text) →
      λ(name : Text) →
      λ(key : Text) →
      λ(bytes : Natural) →
        Secret::{
        , namespace
        , name
        , keys =
          [ { mapKey = key
            , mapValue = Value.Random { random = bytes, id = None Text }
            }
          ]
        }

--| The stage that installs the operator: the CRD, then its namespace (created here, since it is the
--| first stage), access and Deployment.
let operatorStage
    : { namespace : Text, image : Text, imagePullPolicy : Optional Text } → Stage.Type
    = λ(v : { namespace : Text, image : Text, imagePullPolicy : Optional Text }) →
        let namespace = v.namespace

        in  Stage::{
        , name = "operator"
        , description = Some "the PolarAgent CRD and the operator that keeps agents alive"
        , installCrd = True
        , shared = True
        , objects =
            k8s
              (   [ kubernetes.Resource.Namespace
                      kubernetes.Namespace::{
                      , metadata = kubernetes.ObjectMeta::{ name = Some namespace }
                      }
                  ]
                # Operator.makeOperator
                    Operator.Operator::{
                    , namespace
                    , image = v.image
                    , imagePullPolicy = v.imagePullPolicy
                    }
              )
        , waits = [ waitForDeployment namespace "polar-operator" ]
        }

in  { Plan
    , Stage
    , Object
    , Value
    , Secret
    , Wait
    , k8s
    , agents
    , waitForDeployment
    , waitForAgent
    , randomSecret
    , operatorStage
    }
