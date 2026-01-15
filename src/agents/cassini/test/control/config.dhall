{-
  TestPlan.dhall

  A load- and integration-testing oriented harness configuration.
  The harness executes phases sequentially.
  Each phase starts from a clean slate unless explicitly configured otherwise.
-}


let Pattern =
      < Drip : { idle_time_seconds : Natural }
      | Burst : { burst_size : Natural, idle_time_seconds : Natural }
      >

let Producer =
      { topic : Text
      , message_size : Natural
      , duration : Natural
      , pattern : Pattern
      }

let Expectation =
      < AtLeast : { messages : Natural, within_seconds : Natural }
      | NoStarvation : { max_idle_seconds : Natural }
      | OrderingPreserved : { key : Text }
      >

let Phase =
      { name : Text
      , producers : List Producer
      , expectations : List Expectation
      }

let TestPlan = { name : Text, phases : List Phase }

let producers =
      [ { topic = "steady"
        , message_size = 4096
        , duration = 60
        , pattern = Pattern.Drip { idle_time_seconds = 1 }
        }
      ]

let expectations = [ Expectation.AtLeast { messages = 1, within_seconds = 30 } ]

let phases = [ { name = "load test", producers, expectations } ]

let plan
    : TestPlan
    = { name = "Polar test plan", phases }

in  plan
