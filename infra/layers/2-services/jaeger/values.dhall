-- infra/layers/2-services/jaeger/values.dhall
--
-- Canonical defaults for the jaeger chart.
-- Targets override only what differs via targets/<target>/overrides.dhall.

{ name           = "jaeger"
, image          = "cr.jaegertracing.io/jaegertracing/jaeger:2.13.0"
, imagePullPolicy = "IfNotPresent"
, serviceType    = "NodePort"
, ports          =
  { http   = 16686
  , traces = 4318
  }
}
