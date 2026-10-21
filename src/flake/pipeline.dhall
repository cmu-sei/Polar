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

let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/e2334448bd4bb6348a467244d474f907d3d0e36d/dhall/prelude.dhall
        sha256:b81e69ef2fe811bc853a8a9a0202c0af802f7cd53c78f95f67083bf3dceee86b

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
      , "openapi-observer"
      , "orchestrator-core"
      , "pipeline-manifest.pinned.json"
      , "polar-scheduler"
      , "polar-scheduler-observer"
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
