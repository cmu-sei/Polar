-- src/agents/build-orchestrator/container-orchestrator.dhall

let Lib = ../containers/container-lib.dhall
let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "build-orchestrator"
  , entrypoint = Some "build-orchestrator"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  }
