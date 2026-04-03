let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/bc1246f3372fbb825de2a85e6f3ca9d0779975d5/dhall/prelude.dhall
        sha256:42b061b5cb6c7685afaf7e5bc6210640d2c245e67400b22c51e6bfdf85a89e06

let List/map = https://prelude.dhall-lang.org/List/map

let Output = Lib.StageOutput

let mkBinaryArtifact
    : Text → Output
    = λ(name : Text) →
        Output.Artifact { name, content_type = Some "elf-binary-set" }

let binaries =
      [ "build-orchestrator"
      , "build-processor"
      , "build.exit"
      , "build.log"
      , "cassini-c2"
      , "cassini-client"
      , "cassini-server"
      , "config-ops"
      , "event-logger"
      , "git-repo-observer"
      , "git-repo-processor"
      , "gitlab-consumer"
      , "gitlab-observer"
      , "harness-controller"
      , "harness-producer"
      , "harness-sink"
      , "jira-consumer"
      , "jira-observer"
      , "kube-consumer"
      , "kube-observer"
      , "mock-adhoc-agent"
      , "mock-agent"
      , "openapi-observer"
      , "orchestrator-core"
      , "pipeline-manifest.pinned.json"
      , "polar-scheduler"
      , "polar-scheduler-observer"
      , "policy-config"
      , "provenance-linker"
      , "provenance-resolver"
      , "scheduler"
      , "web-consumer"
      ]

let attestedBuildArtifacts = List/map Text Output mkBinaryArtifact binaries

let attestedBuildStage
    : Lib.Stage
    = { name = "attested-build"
      , command = "nu ../../scripts/cargo-attested-build.nu"
      , failureMode = Lib.FailureMode.Collect
      , inputs = [ Lib.StageInput.Workspace ]
      , outputs = attestedBuildArtifacts
      , condition = None Text
      , pure = True
      , impurityReason = None Text
      }

let polarPipeline
    : Lib.PipelineConfig
    = { name = "polar-devsecops"
      , artifactDir = "/workspace/pipeline-out"
      , workingDir = "/workspace/src/agents"
      , outputs =
          None
            { artifacts :
                List
                  { name : Text
                  , fromStage : Text
                  , artifact : Text
                  , attestation : Optional Text
                  , verifyMethod : Optional Text
                  }
            , assertions : List { name : Text, fromStage : Text }
            }
      , stages =
          -- Fast gates: fail immediately so the developer gets signal quickly
          --Cargo fmt in particular returns exit 1 whenever it identifies changes need to be made, so we just warn on that
          [ Lib.simpleStage "fmt" "cargo fmt --check" Lib.FailureMode.Collect
          , Lib.simpleStage
              "lint"
              "cargo clippy -- -D warnings"
              Lib.FailureMode.Collect
          , attestedBuildStage
          ]
      }

in  polarPipeline
