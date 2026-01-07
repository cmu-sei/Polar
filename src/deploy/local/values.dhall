-- Some values specific to the environment
let Constants = ../types/constants.dhall

let Cassini = ../types/cassini.dhall

let Agents = ../types/agents.dhall

let cassini
    : Cassini
    = { name = "cassini"
      , image = "registry.sandbox.labz.s-box.org/sei/polar-mirror/cassini:058b2e2f"
      , ports = { http = 3000, tcp = 8080 }
      , tls =
        { certificateRequestName = "cassini-certificate"
        , certificateSpec =
          { commonName = Constants.mtls.commonName
          , dnsNames = [ Constants.cassiniDNSName ]
          , duration = "2160h"
          , issuerRef =
            { kind = "Issuer", name = Constants.mtls.leafIssuerName }
          , renewBefore = "360h"
          , secretName = Constants.CassiniServerCertificateSecret
          }
        }
      }

let clientTlsConfig
    : Agents.ClientTlsConfig
    = { broker_endpoint = Constants.cassiniDNSName
      , server_name = Constants.mtls.commonName
      , client_certificate_path = Constants.mtls.certPath
      , client_key_path = Constants.mtls.keyPath
      , client_ca_cert_path = Constants.mtls.caCertPath
      }

      let linker
          : Agents.ProvenanceLinker
          = { name = Constants.ProvenanceLinkerName
            , image = "registry.sandbox.labz.s-box.org/sei/polar-mirror/polar-linker-agent:0d2a5c44"
            , graph = Constants.graphConfig
            , tls = Constants.commonClientTls
            }

      let resolver
          : Agents.ProvenanceResolver
          = { name = Constants.ProvenanceLinkerName
            , image = "registry.sandbox.labz.s-box.org/sei/polar-mirror/polar-resolver-agent:0d2a5c44"
            , tls = Constants.commonClientTls
            }


let neo4jPorts = { http = 7474, bolt = 7687 }

let neo4jHomePath = "/var/lib/neo4j"

let neo4j =
      { name = "polar-neo4j"
      , hostName = "neo4j"
      , namespace = Constants.GraphNamespace
      , image = "neo4j:5.26.2"
      , configVolume = "neo4j-config-copy"
      , certificatesVolume = "neo4j-certificates"
      , confDir = "/var/lib/neo4j/conf"
      , certDir = "/var/lib/neo4j/certificates"
      , ports = { http = 7474, bolt = 7687 }
      , config = { name = "neo4j-config", path = "/var/lib/neo4j/conf" }
      , volumes =
        { data =
          { name = "polar-db-data"
          , storageClassName = Some "standard"
          , storageSize = "10Gi"
          , mountPath = "/var/lib/neo4j/data"
          }
        , logs =
          { name = "polar-db-logs"
          , storageClassName = Some "standard"
          , storageSize = "10Gi"
          , mountPath = "/var/lib/neo4j/logs"
          }
        }
      }

let neo4jDNSName = "${Constants.neo4jServiceName}.${Constants.GraphNamespace}.svc.cluster.local"

let neo4jBoltAddr = "neo4j://${neo4jDNSName}:${Natural/show neo4j.ports.bolt}"

let jaeger = {
    name = "jaeger"
    , image = "cr.jaegertracing.io/jaegertracing/jaeger:2.13.0"
    , ports = { http = 16686, grpc = 14250, thrift = 6831 }
    , service = { name = "jaeger-svc" }
    --, volumes = { config = { name = "jaeger-config", path = "/var/lib/jaeger/config" } }
    --, config = { name = "jaeger-config", path = "/var/lib/jaeger/config" }
    --, env = [ kubernetes.EnvVar::{ name = "COLLECTOR_OTLP_ENABLED", value = Some "true" } ]
}

let jaegerDNSName = "http://${jaeger.service.name}.${Constants.PolarNamespace}.svc.cluster.local:${Natural/show jaeger.ports.http}/v1/traces"

in  { cassini, linker, resolver, clientTlsConfig, neo4j , neo4jDNSName, neo4jBoltAddr, jaegerDNSName }
