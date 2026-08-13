--| Example resolver configuration intended for local testing, used as part of the default.yaml file.
--
--  Render with:
--    dhall-to-yaml --preserve-null --file ./example.dhall > resolver.yaml
--
--  Note the `./render.dhall` application on the last line. It is not optional
--  and it is not decoration. `dhall-to-yaml` erases union tags, so rendering a
--  bare `S.Config::{...}` value emits `tls.protocol` as an untagged list and
--  `unqualifiedRefs` as a nested map -- structurally valid YAML that the
--  resolver rejects with `unknown field `protocol``. Every file in this
--  directory that is meant to be handed to `dhall-to-yaml` ends with the
--  renderer applied, precisely so there is nothing here that can be rendered
--  into an invalid config by accident.
--
--  `--preserve-null` matters too: without it `dhall-to-yaml` omits `None`
--  fields entirely rather than emitting `null`. The Rust loader tolerates
--  either, but emitting the keys keeps the output self-documenting.

let S = ./schema.dhall

let config =
      S.Config::{
      , unqualifiedRefs =
          S.UnqualifiedRefPolicy.Qualify
            { registry = "docker.io", namespace = Some "library" }
      , tls =
              S.Config.default.tls
          //  { protocol =
                  S.Protocol.HttpsExcept
                    [ "localhost:31500" ]
              }
      , mirrors = [
        { upstream = "registry.local-registry.svc.cluster.local:5000"
          , mirror = "localhost:31500"
          }
      ] : List S.Mirror
      }

in  ./render.dhall config
