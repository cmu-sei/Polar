{-
  examples/plan.dhall — a plan: the operator, then one agent with a secret it reads.

  Render with:  dhall-to-yaml --file plan.dhall > plan.yaml      (or `polar render plan.dhall`)
  Inspect with: polar plan plan.yaml
  Run with:     polar up plan.yaml --context <kube-context>
-}
let Polar = ../types/package.dhall

let kubernetes = ../types/kubernetes.dhall

let P = Polar.plan

let B = Polar.builder

let platform =
      B.Platform::{
      , cassiniAddr = "cassini-ip-svc.polar.svc.cluster.local:8080"
      , cassiniServerName = "cassini.polar.serviceaccount.cluster.local"
      , issuer =
          B.Issuer.CredentialsAgent
            "http://credentials-agent-observer.polar.svc.cluster.local:8080"
      , polarInitImage = "registry.example/polar-init:latest"
      }

let example =
      B.BuilderAgent::{
      , name = "example"
      , namespace = "example"
      , image = "registry.example/example:latest"
      , settings =
        [ kubernetes.EnvVar::{
          , name = "EXAMPLE_PASSWORD"
          , valueFrom = Some kubernetes.EnvVarSource::{
            , secretKeyRef = Some kubernetes.SecretKeySelector::{
              , name = Some "example-secret"
              , key = "password"
              }
            }
          }
        ]
      }

in  { name = "example"
    , stages =
      [ P.operatorStage
          { namespace = "polar-operator"
          , image = "registry.example/polar-operator:latest"
          , imagePullPolicy = None Text
          }
      , P.Stage::{
        , name = "example"
        , description = Some "the example agent and the secret it reads"
        , secrets = [ P.randomSecret "example" "example-secret" "password" 16 ]
        , objects =
              P.k8s
                [ kubernetes.Resource.Namespace
                    kubernetes.Namespace::{
                    , metadata = kubernetes.ObjectMeta::{ name = Some "example" }
                    }
                , kubernetes.Resource.ServiceAccount
                    (B.makeServiceAccount "example" "example")
                ]
            # P.agents [ B.makePolarAgent platform example ]
        , waits = [ P.waitForAgent "example" "example" ]
        }
      ]
    }
