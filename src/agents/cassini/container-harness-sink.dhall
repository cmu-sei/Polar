-- src/agents/cassini/container-harness-sink.dhall
let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/e2334448bd4bb6348a467244d474f907d3d0e36d/dhall/prelude.dhall
        sha256:b81e69ef2fe811bc853a8a9a0202c0af802f7cd53c78f95f67083bf3dceee86b

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "harness-sink"
  , entrypoint = Some "harness-sink"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  }
