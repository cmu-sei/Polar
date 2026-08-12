-- infra/layers/2-services/ace/values.dhall
--
-- Canonical defaults for the ace chart.
-- Targets override only what differs via targets/<target>/overrides.dhall.

{ name             = "ace"
, image            = "ace-viz:latest"
, imagePullPolicy  = "IfNotPresent"
, imagePullSecrets = [] : List { name : Optional Text }
, serviceAccountName = "ace"
, serviceType      = "ClusterIP"
, port             = 8080
}
