let Types = ../types.dhall

in  { agentType = "user-update-handler"
    , eventPattern = "events.user.updated"
    , defaultSchedule = None Types.TimeSpec   -- run immediately
    , config = { outputTopic = "graph.updates", severityThreshold = Some 5 }
    , metadata = Some { description = "Handles user update events"
                      , owner = "team-c"
                      , version = Some 1
                      }
    }
