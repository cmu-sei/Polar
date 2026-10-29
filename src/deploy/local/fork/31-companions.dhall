{-
  What the platform's agents stand on: the identity each runs as (the name on its certificate, and for
  the credentials agent a name the issuer is configured to trust), the ledger's claim, and the Services
  clients dial. Apply with, and before, 30-agents.
-}
let Polar = ../../types/package.dhall

let C = ../../types/lib-constants.dhall

let kubernetes = ../../types/kubernetes.dhall

let values = ../values-fork.dhall

let B = Polar.builder

let ns = values.namespace

in  [ kubernetes.Resource.ServiceAccount (B.makeServiceAccount ns "cassini")
    , kubernetes.Resource.ServiceAccount (B.makeServiceAccount ns "credentials-agent")
    , kubernetes.Resource.ServiceAccount
        (B.makeServiceAccount ns "credentials-agent-observer")
    , kubernetes.Resource.PersistentVolumeClaim
        (B.makePvc ns "credentials-agent-data" "1Gi" values.storageClass)
    -- The name clients dial, which is not the agent's: the server certificate carries it.
    , kubernetes.Resource.Service
        (B.makeServiceFor ns C.cassiniServiceName "cassini" C.cassiniTcpPort)
    , kubernetes.Resource.Service
        (B.makeService ns "credentials-agent-observer" 8080)
    ]
