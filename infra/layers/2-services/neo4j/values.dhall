-- infra/layers/2-services/neo4j/values.dhall
--
-- Canonical defaults for the neo4j chart.
-- Targets override only what differs via targets/<target>/overrides.dhall.
--
-- Secrets (NEO4J_AUTH, TLS cert/key content) are not here — they are
-- threaded in by render.nu at render time from secrets/<layer>/neo4j/.

{ name            = "polar-neo4j"
, hostName        = "neo4j"
, namespace       = "polar-graph"
, image           = "neo4j:5.26.2"
, imagePullPolicy = "IfNotPresent"
, enableTls       = True
, configVolume    = "neo4j-config-copy"
, ports           =
  { http  = 7474
  , https = 7473
  , bolt  = 7687
  }
, config =
  { name = "neo4j-config"
  , path = "/var/lib/neo4j/conf"
  }
, volumes =
  { data =
    { name             = "polar-db-data"
    , storageClassName = Some "managed-csi"
    , storageSize      = "10Gi"
    , mountPath        = "/var/lib/neo4j/data"
    }
  , logs =
    { name             = "polar-db-logs"
    , storageClassName = Some "managed-csi"
    , storageSize      = "10Gi"
    , mountPath        = "/var/lib/neo4j/logs"
    }
  , certs =
    { name             = "polar-db-certs"
    , storageClassName = Some "managed-csi"
    , storageSize      = "1Gi"
    , mountPath        = "/var/lib/neo4j/certificates"
    }
  }
}
