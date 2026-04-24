-- src/agents/cassini/container-harness-sink.dhall
let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/d8703888ed01e53b30cb093d15e13c0566df6384/dhall/prelude.dhall
        sha256:f75818ad203cb90a5e5921b75cd60bcb66ac5753cf7eba976538bf71e855378c

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "harness-sink"
  , entrypoint = Some "harness-sink"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  }
