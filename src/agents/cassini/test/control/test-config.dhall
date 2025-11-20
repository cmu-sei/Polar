let types = ./conf/types.dhall



let producerConfig = "./conf/producer.dhall"

let plan
    : types.TestPlan
    = { environment =
        {
        cassini =
            {
            tag = "latest"
            -- paths to mount TLS certificates
            , ca_cert_path = env:TLS_CA_CERT as Text
            , server_cert_path = env:TLS_SERVER_CERT_CHAIN as Text
            , server_key_path = env:TLS_SERVER_KEY as Text
            , jager_host = None Text
            , log_level = None Text
            }
        , neo4j = { enable = True, version = "5.26", config = None Text }
        , registry = { enable = True, version = "2.8", port = 5000 }
        }
      , phases =
        [ { name = "start agents"
          , actions =
            [
            , types.Action.StartAgent
                { agent = types.Agent.ProducerAgent, config = Some producerConfig }
            , types.Action.StartAgent
                { agent = types.Agent.SinkAgent, config = None Text }
            ]
              --[ types.Action.StartActor
              --    { agent = types.Agent.GitlabConsumer, config = None Text }
              --, types.Action.StartActor
              --    { agent = types.Agent.ResolverAgent, config = None Text }
              --, types.Action.StartActor
              --    { agent = types.Agent.LinkerAgent, config = None Text }
              --] : List types.Action
          }
          , {
            name = "await stress test"
            , actions = [
                types.Action.Sleep { duration = None Natural }
            ]
          }
        ]
      }

in  plan
