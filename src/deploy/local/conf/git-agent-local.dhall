-- simple git agent config that denotes a public repo with no authentication required, used to demonstrate configuration
--
let GitAgent = ../../../agents/git/observe/conf/git.dhall

in    { repos =
        [ { mapKey = "https://github.com/cmu-sei/Polar.git"
          , mapValue = GitAgent.noAuth
          }
        ]
      }
    : GitAgent.GitAgentConfig
