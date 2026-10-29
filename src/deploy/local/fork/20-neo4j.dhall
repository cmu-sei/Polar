{-
  A graph with a password, for development. Optional for Blitz's portal and entitlement service; the
  investigation engine reads from it, and the credentials agent's graph section is pointed at it.
  Needs the Secret `neo4j-secret` (`neo4j/<password>`).
-}
let Polar = ../../types/package.dhall

let values = ../values-fork.dhall

in  Polar.platform.makeNeo4j
      Polar.platform.Neo4j::{
      , namespace = values.namespace
      , image = values.images.neo4j
      , imagePullPolicy = values.imagePullPolicy
      , storageClass = values.storageClass
      }
