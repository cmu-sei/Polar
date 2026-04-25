-- src/agents/cassini/container-harness-producer.dhall

let Lib = ../../containers/container-lib.dhall
let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "harness-producer"
  , entrypoint = Some "harness-producer"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  }
