{-
  local/fork/plan.dhall — the fork's Polar platform on a local cluster, as a plan.

  What a builder agent (Blitz's, for one) connects to: the operator that keeps agents alive, the
  certificate issuer, Cassini, the credentials agent and its observer, and a graph.

    polar plan local/fork/plan.dhall            what it would do
    polar up   local/fork/plan.dhall --context <kube-context> [--with graph]

  The stages are the order the platform needs: the operator and its CRD; the issuer, which every pod's
  init container asks for a certificate; then Cassini and the credentials agent, which start with
  certificates from the issuer directly. The graph is optional.

  The unit expressions beside this file (00..31) are what each stage is made of. Secrets are made when
  absent, with random values that are never printed.
-}
let Polar = ../../types/package.dhall

let P = Polar.plan

let values = ../values-fork.dhall

let ns = values.namespace

in  { name = "polar-platform"
    , stages =
      [ P.operatorStage
          { namespace = "polar-operator"
          , image = values.images.operator
          , imagePullPolicy = values.imagePullPolicy
          }
      , P.Stage::{
        , name = "issuer"
        , description = Some "the polar namespace and the certificate issuer"
        , secrets =
          [ P.randomSecret ns "cassini-shutdown-token" "token" 24
          -- The credentials agent reads graph settings even where no graph runs, so the password
          -- exists either way. The graph's own secret is its `neo4j/<password>` form, and the two
          -- are made together so they cannot disagree.
          , P.Secret::{
            , namespace = ns
            , name = "polar-graph-pw"
            , group = Some "graph"
            , keys =
              [ { mapKey = "secret"
                , mapValue = P.Value.Random { random = 16, id = Some "password" }
                }
              ]
            }
          , P.Secret::{
            , namespace = ns
            , name = "neo4j-secret"
            , group = Some "graph"
            , keys =
              [ { mapKey = "secret"
                , mapValue = P.Value.Template { template = "neo4j/{password}" }
                }
              ]
            }
          ]
        , objects = P.k8s (./00-namespace.dhall # ./10-cert-issuer.dhall)
        , waits = [ P.waitForDeployment ns "cert-issuer" ]
        }
      , P.Stage::{
        , name = "platform"
        , description = Some "Cassini, the credentials agent and its observer"
        , objects = P.k8s ./31-companions.dhall # P.agents ./30-agents.dhall
        , waits =
          [ P.waitForAgent ns "cassini"
          , P.waitForAgent ns "credentials-agent"
          , P.waitForAgent ns "credentials-agent-observer"
          ]
        }
      , P.Stage::{
        , name = "graph"
        , description = Some "a graph with a password, for the investigation engine"
        , optional = True
        , objects = P.k8s ./20-neo4j.dhall
        , waits = [ P.waitForDeployment ns "polar-neo4j" ]
        }
      ]
    }
