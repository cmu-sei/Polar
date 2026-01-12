let SessionExpectation = < CountAtLeast : Natural | ContainsIds : List Text >

let TopicExpectation = < Exists : List Text | CountAtLeast : Natural >

let SubscriptionExpectation = { topic : Text, countAtLeast : Natural }

let Expectations =
      { sessions : Optional SessionExpectation
      , topics : Optional TopicExpectation
      , subscriptions : Optional SubscriptionExpectation
      }

let Gate =
      { description : Optional Text
      , timeoutSeconds : Natural
      , expect : Expectations
      }

let Pattern =
      < Drip : { idle_time_seconds : Natural }
      | Burst : { burst_size : Natural, idle_time_seconds : Natural }
      >

let PayloadSpec =
      < Fixed : Text
      | Random : { seed : Natural }
      | FromFile : { path : Text }
      | Template : { template : Text }
      >

let Producer =
      { topic : Text
      , msgSize : Natural
      , messageCount : Natural
      , durationSeconds : Natural
      , pattern : Pattern
      , payload : PayloadSpec
      }

let StartProducer = { clientId : Text, producer : Producer }

let Subscribe = { clientId : Text, topic : Text }

let Unsubscribe = { clientId : Text, topic : Text }

let Disconnect = { clientId : Text }

let Action =
      < StartProducer : StartProducer
      | Subscribe : Subscribe
      | Unsubscribe : Unsubscribe
      | Disconnect : Disconnect
      >

let Step = < Wait : Gate | Do : Action >

let TestPlan =
      { name : Text
      , description : Optional Text
      , timeoutSeconds : Natural
      , steps : List Step
      }

let steadyProducer
    : Producer
    = { topic = "steady"
      , msgSize = 4096
      , messageCount = 0
      , durationSeconds = 60
      , pattern = Pattern.Drip { idle_time_seconds = 1 }
      , payload = PayloadSpec.Random { seed = 42 }
      }

let expectController
    : Expectations
    = { sessions = Some (SessionExpectation.CountAtLeast 1)
      , topics = None TopicExpectation
      , subscriptions = None SubscriptionExpectation
      }

let steps =
      [ Step.Wait
          { description = Some "Wait for producer and sink to register"
          , timeoutSeconds = 15
          , expect = expectController
          }
      ]

let plan
    : TestPlan
    = { name = "plan", description = Some "Desc", timeoutSeconds = 60, steps }

in  plan
