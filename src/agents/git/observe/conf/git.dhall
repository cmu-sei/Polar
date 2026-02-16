{- ============================================================================
   Git Agent Static Credential Configuration
   ----------------------------------------------------------------------------
-}

let HttpCredential =
      { username : Text
      , token : Text
      }

let HostCredentialConfig =
      { http : Optional HttpCredential
      }

let StaticCredentialConfig =
      { hosts : List
          { mapKey : Text
          , mapValue : HostCredentialConfig
          }
      }
-- read creds from env
let username = env:GITLAB_USER as Text
let token = env:GITLAB_TOKEN as Text

-- generate config, hardcoded hosts for now.
in  { hosts =
        [ { mapKey = "gitlab.sandbox.labz.s-box.org"
          , mapValue =
              { http =
                  Some
                    { username = username
                    , token = token
                    }
              }
          }
        ]
    }
    : StaticCredentialConfig
