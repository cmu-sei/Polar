{-
  examples/builder-companions.dhall — what goes beside builder-agent.dhall: the identity the agent
  runs as, the Service that fronts it and the claim that keeps its state.

  Render with:  dhall-to-yaml --documents --file builder-companions.dhall > builder-companions.yaml
-}
let Polar = ../types/package.dhall

let kubernetes = ../types/kubernetes.dhall

let B = Polar.builder

in  [ kubernetes.Resource.ServiceAccount (B.makeServiceAccount "example" "example")
    , kubernetes.Resource.Service (B.makeService "example" "example" 8080)
    , kubernetes.Resource.PersistentVolumeClaim
        (B.makePvc "example" "example-data" "1Gi" (None Text))
    ]
