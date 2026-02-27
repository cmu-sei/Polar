let TimeSpec = < Periodic : { interval : Natural, unit : < Minutes | Hours | Days > }
               | Exact : { timestamp : Text }
               | Daily : { hour : Natural, minute : Natural, timezone : Optional Text }
               | Weekly : { day : < Mon | Tue | Wed | Thu | Fri | Sat | Sun >,
                            hour : Natural, minute : Natural, timezone : Optional Text }
               | Monthly : { day : Natural, hour : Natural, minute : Natural,
                             timezone : Optional Text }
               >

let ScheduleKind = < Permanent | Adhoc | Ephemeral >

let ScheduleMetadata = { description : Optional Text, owner : Optional Text }

in  { id = "adhoc/git-repo-observer"
    , kind = ScheduleKind.Adhoc
    , agent_id = None Text
    , agent_type = Some "git-repo-observer"
    , schedule = TimeSpec.Periodic { interval = 5, unit = < Minutes | Hours | Days >.Minutes }
    , config = { repoUrl = "https://example.com/repo.git", branch = "main" }
    , metadata = { description = Some "Observes a git repo every 5 minutes", owner = Some "test" }
    , version = 1
    }
