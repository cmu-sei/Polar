-- src/flake/ci-container.dhall
--
-- Polar CI container configuration.
-- Headless pipeline runner — no interactive shell, minimal package set.
-- The same pipeline definition as the dev container guarantees that what
-- runs locally and what runs in CI are identical.

let Lib = ../containers/contaienr-lib.dhall
let defaults = Lib.defaults

-- ---------------------------------------------------------------------------
-- Pipeline definition — identical to the dev container's.
-- Single source of truth. Do not diverge.
-- ---------------------------------------------------------------------------
let polarPipeline : Lib.PipelineConfig =
  { name        = "polar-devsecops"
  , artifactDir = "/workspace/pipeline-out"
  , workingDir  = "/workspace/src/agents"
  , outputs     = None { artifacts : List { name : Text, fromStage : Text, artifact : Text, attestation : Optional Text, verifyMethod : Optional Text }, assertions : List { name : Text, fromStage : Text } }
  , stages      =
      [ Lib.simpleStage "fmt"  "cargo fmt --check"           Lib.FailureMode.Collect
      , Lib.simpleStage "lint" "cargo clippy -- -D warnings" Lib.FailureMode.Collect
      , { name           = "static-analysis"
        , command        = "run-analysis --config ./analysis.toml"
        , failureMode    = Lib.FailureMode.Collect
        , inputs         = [ Lib.StageInput.Workspace ]
        , outputs        = [ Lib.StageOutput.Report { name = Some "static-analysis-report" } ]
        , condition      = None Text
        , pure           = True
        , impurityReason = None Text
        }
      , { name           = "audit"
        , command        = "run-audit --sbom ./sbom.json"
        , failureMode    = Lib.FailureMode.Collect
        , inputs         = [ Lib.StageInput.Workspace ]
        , outputs        =
            [ Lib.StageOutput.Report { name = Some "audit-report" }
            , Lib.StageOutput.Artifact { name = "sbom", content_type = Some "application/json" }
            ]
        , condition      = None Text
        , pure           = True
        , impurityReason = None Text
        }
      , Lib.conditionalStage
          "full-test"
          "cargo test --workspace"
          Lib.FailureMode.FailFast
          "CI_FULL"
      ]
  }

let polarCiExtras =
  Lib.customLayer "polar-ci-extras"
    [ Lib.flakePackage "staticanalysis" "default"
    , Lib.flakePackage "cassini-client" "default"
    ]

let polarEnv : List Lib.EnvVar =
  [ Lib.buildEnv "GRAPH_DB"       "neo4j"
  , Lib.buildEnv "GRAPH_ENDPOINT" "bolt://127.0.0.1:7687"
  , Lib.buildEnv "GRAPH_USER"     "neo4j"
  ]

in defaults.ciContainer //
  { name = "polar-ci"

  , packageLayers =
      [ Lib.PackageLayer.Micro
      , Lib.PackageLayer.Core
      , Lib.PackageLayer.CI
      , Lib.PackageLayer.RustToolchain
      , polarCiExtras
      ]

  , pipeline = Some polarPipeline

  , extraEnv = polarEnv
  }
