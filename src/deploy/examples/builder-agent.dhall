{-
  examples/builder-agent.dhall — one agent built on polar-agent-builder, as a PolarAgent resource.

  Render with:  dhall-to-yaml --documents --file builder-agent.dhall > builder-agent.yaml
  (the committed builder-agent.yaml is the golden output).

  The Service, claim and ServiceAccount that go with it are a different kind of list, and render
  from builder-companions.dhall.
-}
let Polar = ../types/package.dhall

let kubernetes = ../types/kubernetes.dhall

let B = Polar.builder

-- Facts about the cluster, stated once for every agent.
let platform =
      B.Platform::{
      , cassiniAddr = "cassini-ip-svc.polar.svc.cluster.local:8080"
      , cassiniServerName = "cassini.polar.serviceaccount.cluster.local"
      -- Certificates are asked of the credentials agent, which vouches for the request.
      , issuer =
          B.Issuer.CredentialsAgent
            "http://credentials-agent-observer.polar.svc.cluster.local:8080"
      , polarInitImage = "registry.example/polar-init:latest"
      }

-- What this agent is. Everything else comes from BuilderAgent.default.
let example =
      B.BuilderAgent::{
      , name = "example"
      , namespace = "example"
      , image = "registry.example/example:latest"
      , shutdownBudgetSecs = 30
      , port = Some 8080
      , data = Some { claim = "example-data", mountPath = "/data" }
      , settings =
        [ kubernetes.EnvVar::{ name = "EXAMPLE_REALM", value = Some "demo" }
        , kubernetes.EnvVar::{
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

in  [ B.makePolarAgent platform example ]
