let Types = ../types.dhall

in  { agentId = "jenkins-observer-1"
    , schedule = Types.TimeSpec.Periodic { interval = 5, unit = < minutes | hours | days >.minutes }
    , rejuvenation = Some { interval = 24, unit = < minutes | hours | days >.hours }
    , config = { jenkinsUrl = "https://jenkins.example.com"
               , jobPattern = Some ".*"
               , outputTopic = "observations.jenkins"
               , credentials = Types.Credentials.reference "jenkins-cred"
               }
    , metadata = Some { description = "Observes all Jenkins jobs every 5 minutes"
                      , owner = "team-a"
                      , version = Some 1
                      }
    }
