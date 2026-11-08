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
            are made together, sharing the random values they name, or not at all. A key may be half of
            a certificate authority made for it (`certificateAuthority`): the cluster's own CA is made
            by the plan, once, and never replaced by running it again
    ConfigMap  written every run to hold a literal or a copy of a Secret's key, so the public half of
            the CA in each namespace follows the Secret it came from
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
      -- Half of a certificate authority made for the secret, `part` being "certificate" or "key". Keys with
      -- the same `id` in one secret (or one group) are one pair, made together.
      | Ca :
          { ca :
              { id : Text
              , part : Text
              , commonName : Text
              , validityDays : Natural
              }
          }
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

--| A ConfigMap of the plan: unlike a Secret it is written every time, and it holds only a literal or
--| a copy of a key of a Secret, since a ConfigMap is readable by every pod in its namespace.
let ConfigMap =
      { Type =
          { namespace : Text
          , name : Text
          , keys : List { mapKey : Text, mapValue : Value }
          }
      , default = {=}
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
          , configMaps : List ConfigMap.Type
          , objects : List Object
          , waits : List Wait.Type
          }
      , default =
        { description = None Text
        , optional = False
        , shared = False
        , installCrd = False
        , secrets = [] : List Secret.Type
        , configMaps = [] : List ConfigMap.Type
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

--| A certificate authority, made with the secret and kept in it: `ca.crt` and `ca.key`, the names the
--| certificate issuer loads. Made once, when the secret is absent; the key never leaves the secret.
let certificateAuthority
    : { namespace : Text, name : Text, commonName : Text, validityDays : Natural } → Secret.Type
    = λ(v : { namespace : Text, name : Text, commonName : Text, validityDays : Natural }) →
        let half =
              λ(part : Text) →
                Value.Ca
                  { ca =
                    { id = "root"
                    , part
                    , commonName = v.commonName
                    , validityDays = v.validityDays
                    }
                  }

        in  Secret::{
            , namespace = v.namespace
            , name = v.name
            , keys =
              [ { mapKey = "ca.crt", mapValue = half "certificate" }
              , { mapKey = "ca.key", mapValue = half "key" }
              ]
            }

--| The certificate half of a certificate authority (`certificateAuthority`), in a ConfigMap of the namespace
--| of every pod that verifies certificates against it, as `ca.pem`.
let pinnedCa
    : { namespace : Text, name : Text, secret : { namespace : Text, name : Text } } → ConfigMap.Type
    = λ(v : { namespace : Text, name : Text, secret : { namespace : Text, name : Text } }) →
        ConfigMap::{
        , namespace = v.namespace
        , name = v.name
        , keys =
          [ { mapKey = "ca.pem"
            , mapValue =
                Value.Copy
                  { copy = { namespace = v.secret.namespace, name = v.secret.name, key = "ca.crt" } }
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
    , ConfigMap
    , Wait
    , k8s
    , agents
    , waitForDeployment
    , waitForAgent
    , randomSecret
    , certificateAuthority
    , pinnedCa
    , operatorStage
    }
