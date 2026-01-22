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

let Test =
      { name : Text
      , producer : Producer
      --, expectations : List Expectation
      }

let TestPlan = { name : Text, tests : List Test }

--TODO: Rather than provide a list of producer configuration, which really act as
--multiple actors. We should have just one producer configuration that should contain the same params
--This will simplify the producer agent, as now it will not have to supervise multiple "producer" actors
--that all used the same client.
let producer =
       { topic = "steady"
        , message_size = 4096
        , duration = 3
        , pattern = Pattern.Drip { idle_time_seconds = 1 }
        }


--let expectations = [ Expectation.AtLeast { messages = 1, within_seconds = 30 } ]

let tests = [ { name = "load test", producer } ]

let plan
    : TestPlan
    = { name = "Polar test plan", tests }

in  plan
