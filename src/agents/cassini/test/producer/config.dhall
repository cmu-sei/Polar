-- TestPlan.dhall

-- Do we need this?

let MessagePayload = { message: Text , checksum: Optional Text }

-- Pattern for message emission
-- When configured to drip messages, they'll be sent at a constant rate denoted by idle_time between messages,
-- When configured to send in bursts, the burst_size denotes the amount of messages sent between idle_times
let Pattern =
      < Drip: { idle_time: Natural }
      | Burst :
          { burst_size : Natural
          , idle_time  : Natural
          }
      >

-- A single producer configuration
let Producer =
      { topic     : Text
      , msgSize   : Natural
      , duration  : Natural
      , pattern   : Pattern
      }



-- A test plan is just a list of producers
-- TODO: We've discussed potentially splitting the producer and sink components again to enable them to communicate over the wire
-- This is probably where some of those configurations would be best suited.
let TestPlan = {
    producers : List Producer
}

let steadyProducer: Producer = {
   topic = "steady"
,  msgSize = 4096
,  duration = 30
,  pattern = Pattern.Drip { idle_time = 30 }
}

let burstProducer: Producer = {
   topic = "burst"
,  msgSize = 4096
,  duration = 60
,  pattern = Pattern.Burst { burst_size = 10, idle_time = 5 }
}


let plan: TestPlan = {
    producers = [ steadyProducer, burstProducer ]
}

in plan
