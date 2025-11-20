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


--define some producers, we'll derive sink configuration from here.
let steadyProducer: Producer = {
   topic = "steady"
,  msgSize = 4096
,  duration = 60
,  pattern = Pattern.Drip { idle_time = 1 }
}

let producers = [ steadyProducer ]

in producers
