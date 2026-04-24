-- src/agents/build-orchestrator/container-processor.dhall
let Lib =
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/1d33798e2764db180f9ec0e3977397a52178c717/dhall/prelude.dhall
        sha256:f75818ad203cb90a5e5921b75cd60bcb66ac5753cf7eba976538bf71e855378c

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "build-processor"
  , entrypoint = Some "build-processor"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  }
