-- src/flake/container.dhall
--
-- Polar dev container configuration.
-- This file is the single place where polar-specific container decisions live.
-- All the how is in nix-container-lib. This file is pure what.
--
-- Library reference: github:daveman1010221/nix-container-lib

let Lib      = PRELUDE_PATH
let defaults = Lib.defaults

-- ---------------------------------------------------------------------------
-- Pipeline definition
-- The same pipeline runs locally (developer) and in CI (server).
-- That identity is the guarantee. Do not create a separate CI pipeline.
-- ---------------------------------------------------------------------------
let polarPipeline : Lib.PipelineConfig =
  { name        = "polar-devsecops"
  , artifactDir = "/workspace/pipeline-out"
  , workingDir  = "/workspace/src/agents"
  , stages      =

      -- Fast gates: fail immediately so the developer gets signal quickly
      [ Lib.simpleStage "fmt"  "cargo fmt --check"           Lib.FailureMode.Collect
      , Lib.simpleStage "lint" "cargo clippy -- -D warnings" Lib.FailureMode.Collect

      -- Static analysis: collect all findings before reporting
      ,   { name        = "static-analysis"
          , command     = "run-analysis --config ./analysis.toml"
          , failureMode = Lib.FailureMode.Collect
          , inputs      = [ Lib.StageInput.Workspace ]
          , outputs     = [ Lib.StageOutput.Report ]
          , condition   = None Text
          }

      -- Audit: produces both a report and a named artifact for downstream
      ,   { name        = "audit"
          , command     = "run-audit --sbom ./sbom.json"
          , failureMode = Lib.FailureMode.Collect
          , inputs      = [ Lib.StageInput.Workspace ]
          , outputs     =
              [ Lib.StageOutput.Report
              , Lib.StageOutput.Artifact "sbom"
              ]
          , condition   = None Text
          }

      -- Full test suite: gated on CI_FULL so developers run it explicitly
      -- while CI always runs it. Same definition, different invocation scope.
      , Lib.conditionalStage
          "full-test"
          "cargo test --workspace"
          Lib.FailureMode.FailFast
          "CI_FULL"
      ]
  }

-- ---------------------------------------------------------------------------
-- Polar-specific packages not covered by standard layers.
-- These come from external flake inputs wired in flake.nix.
-- The flakeInput names must match the input names in flake.nix exactly.
-- ---------------------------------------------------------------------------
let polarExtras =
  Lib.customLayer "polar-extras"
    [ Lib.flakePackage "staticanalysis"  "default"
      , Lib.flakePackage "dotacat"       "default"
      , Lib.nixpkgs "nvim-pkg"
    ]

-- ---------------------------------------------------------------------------
-- Project-specific environment variables.
-- BuildTime: arch-independent, no store paths — safe for config.Env.
-- ---------------------------------------------------------------------------
let polarEnv : List Lib.EnvVar =
  [ Lib.buildEnv "GRAPH_DB"       "neo4j"
  , Lib.buildEnv "GRAPH_ENDPOINT" "bolt://127.0.0.1:7687"
  , Lib.buildEnv "GRAPH_USER"     "neo4j"
  ]

-- ---------------------------------------------------------------------------
-- The container configuration.
-- Derived from defaults.devContainer with polar-specific overrides.
-- ---------------------------------------------------------------------------
in defaults.devContainer //
  { name = "polar-dev"

  , packageLayers =
      [ Lib.PackageLayer.Core
      , Lib.PackageLayer.CI
      , Lib.PackageLayer.Dev
      , Lib.PackageLayer.Toolchain
      , Lib.PackageLayer.Pipeline
      , polarExtras
      ]

  , pipeline = Some polarPipeline

  , tls = Some
      ( defaults.defaultTLS //
        { generateCerts = True }
      )

  , ssh = Some
      ( defaults.defaultSSH //
        { enable = False
        , port   = 2223
        }
      )

  , extraEnv = polarEnv
  }
