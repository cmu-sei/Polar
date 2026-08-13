-- src/agents/cassini/container-harness-sink.dhall

let Lib = ../../containers/container-lib.dhall
let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "harness-sink"
  , entrypoint = Some "harness-sink"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  }
