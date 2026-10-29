{-
  examples/operator.dhall — the polar-operator, in a namespace of its own.

  Render with:  dhall-to-yaml --documents --file operator.dhall > operator.yaml
  The CRD comes from the operator itself:  polar-operator crd | kubectl apply -f -
-}
let Polar = ../types/package.dhall

in  Polar.operator.makeOperator
      Polar.operator.Operator::{
      , namespace = "polar-operator"
      , image = "registry.example/polar-operator:latest"
      }
