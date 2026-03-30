-- src/flake/ci-container.dhall
--
-- Polar CI container configuration.
-- Headless pipeline runner — no interactive shell, minimal package set.
-- The same pipeline definition as the dev container guarantees that what
-- runs locally and what runs in CI are identical.
--
-- To update the pinned prelude after a nix-container-lib release:
--   nix-prefetch-git https://github.com/daveman1010221/nix-container-lib
--   dhall hash <<< "https://raw.githubusercontent.com/daveman1010221/nix-container-lib/<rev>/dhall/prelude.dhall"

let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/bc1246f3372fbb825de2a85e6f3ca9d0779975d5/dhall/prelude.dhall
        sha256:42b061b5cb6c7685afaf7e5bc6210640d2c245e67400b22c51e6bfdf85a89e06

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
    ]

let polarEnv : List Lib.EnvVar =
  [ Lib.buildEnv "GRAPH_DB"       "neo4j"
  , Lib.buildEnv "GRAPH_ENDPOINT" "bolt://127.0.0.1:7687"
  , Lib.buildEnv "GRAPH_USER"     "neo4j"
  ]

in defaults.pipelineContainer //
  { name = "polar-ci"

  , packageLayers =
      [ Lib.PackageLayer.Core
      , Lib.PackageLayer.CI
      , Lib.PackageLayer.Pipeline
      , polarCiExtras
      ]

  , pipeline = Some polarPipeline

  , extraEnv = polarEnv
  }
