let Agents = ../../types/agents.dhall

let config
    : Agents.StaticCredentialConfig
    = { hosts =
        [ { mapKey = "gitlab.sandbox.labz.s-box.org"
          , mapValue.http
            = Some
            { username = env:GITLAB_USER as Text
            , token = env:GITLAB_TOKEN as Text
            }
          }
        ]
      }

in  config
