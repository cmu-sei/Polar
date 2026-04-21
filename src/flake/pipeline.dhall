let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/9da4924831c8e0d81d57448425d6cd10820b71d2/dhall/prelude.dhall
        sha256:18acbbb5708565905ab9522fa77a81eb402851f06870a34a22f6c979001c4571

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
