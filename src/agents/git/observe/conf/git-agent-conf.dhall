let GitAgent = ./git.dhall

in    { repos =
        [ { mapKey = "https://github.com/cmu-sei/Polar.git"
          , mapValue = GitAgent.noAuth
          }
        ]
      }
    : GitAgent.GitAgentConfig
