-- src/agents/build-orchestrator/container-processor.dhall



let Lib = ../containers/container-lib.dhall
let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "build-processor"
  , entrypoint = Some "build-processor"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  }
