let values = ../values.dhall

let Rule = 
  { from : Optional (List { source : { principals : Optional (List Text), notRequestPrincipals : Optional (List Text) } })
  , to : Optional (List { operation : { ports : List Text } })
  }

let rules : List Rule =
  [ { from = Some
      [ { source =
          { principals = Some
              [ "cluster.local/ns/${values.namespace}/sa/${values.gitlab.serviceAccountName}" ]
          , notRequestPrincipals = None (List Text)
          }
        }
      ]
    , to = None (List { operation : { ports : List Text } })
    }

  , { from = Some
      [ { source =
          { principals = None (List Text)
          , notRequestPrincipals = Some [ "*" ]
          }
        }
      ]
    , to = Some
        [ { operation =
            { ports = [ Natural/show values.neo4jPorts.bolt ] }
          }
        ]
    }
  ]

let AllowPolicy =
  { apiVersion = "security.istio.io/v1"
  , kind = "AuthorizationPolicy"
  , metadata =
      { name = "graph-db-restrict-bolt"
      , namespace = values.neo4j.namespace
      }
  , spec =
      { action = "ALLOW"
      , rules = rules
      , selector = { matchLabels = { app = values.neo4j.name } }
      }
  }

in AllowPolicy
