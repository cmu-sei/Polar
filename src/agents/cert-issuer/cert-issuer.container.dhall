-- src/agents/cassini/container-cassini.dhall

let Lib = ../../containers/container-lib.dhall

let defaults = Lib.defaults

in defaults.minimalContainer //
  { name       = "polar-cert-issuer"
  , entrypoint = Some "cert-issuer"
  , staticUid  = Some 1000
  , staticGid  = Some 1000
  , extraEnv   = [ Lib.buildEnv "RUST_LOG" "debug" ]
  }
