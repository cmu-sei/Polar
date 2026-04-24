let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/1d33798e2764db180f9ec0e3977397a52178c717/dhall/prelude.dhall
        sha256:f75818ad203cb90a5e5921b75cd60bcb66ac5753cf7eba976538bf71e855378c

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


let cargoCycloneDXStage
    : Lib.Stage
    = { name = "make-sboms"
      , command = "nu ../../scripts/nushell/cargo-sbom.nu"
      , failureMode = Lib.FailureMode.Collect
      , inputs = [ Lib.StageInput.Workspace ]
      , outputs = attestedBuildArtifacts
      , condition = None Text
      , pure = True
      , impurityReason = None Text
      }

let cargoBuildStage
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

let stages = [
    Lib.simpleStage "lint" "cargo clippy -- -D warnings" Lib.FailureMode.Collect
    , Lib.simpleStage "fmt" "cargo fmt --check" Lib.FailureMode.Collect
    ,    cargoCycloneDXStage
    ,    cargoBuildStage
]

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
      , stages
      }

in  polarPipeline
