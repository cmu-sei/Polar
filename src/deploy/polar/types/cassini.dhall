-- types/Cassini.dhall
{-
  This describes the full configuration needed to deploy Cassini
-}

let kubernetes = ./kubernetes.dhall
let mtls = ../path/to/mtls.dhall

in 
{ Type = 
    { name : Text
    , namespace : Text
    , image : Text
    , imagePullSecrets : List Text
    , port : Natural
    , service : { name : Text, type : Text }
    , tls : 
        { certificateRequestName : Text
        , certificateSpec :
          { commonName : Text
          , dnsNames : List Text
          , duration : Text
          , issuerRef : { kind : Text, name : Text }
          , renewBefore : Text
          , secretName : Text
          }
        }
    , environment : List kubernetes.EnvVar.Type
    , containerSecurityContext : kubernetes.SecurityContext.Type
    , podAnnotations : List Text
    , volumes : List kubernetes.Volume.Type
    , volumeMounts : List kubernetes.VolumeMount.Type
    }
, default =
    λ(namespace : Text) →
    let cassiniPort = 8080

    let cassiniDNSName = "${"cassini-ip-svc"}.${namespace}.svc.cluster.local"

    let cassiniServerCertificateSecret = "cassini-tls"

    in 
    { name = "cassini"
    , namespace = namespace
    , image = "${sandboxRegistry.url}/polar/cassini:${chart.appVersion}"
    , imagePullSecrets = sandboxRegistry.imagePullSecrets
    , port = cassiniPort
    , service = { name = "cassini-ip-svc", type = "ClusterIP" }
    , tls = {
        certificateRequestName = "cassini-certificate",
        certificateSpec = {
            commonName = mtls.commonName,
            dnsNames = [ cassiniDNSName ],
            duration = "2160h",
            issuerRef = { kind = "Issuer", name = mtls.leafIssuerName },
            renewBefore = "360h",
            secretName = cassiniServerCertificateSecret
        }
      }
    , containerSecurityContext = 
        kubernetes.SecurityContext::{
        , runAsGroup = Some 1000
        , runAsNonRoot = Some True
        , runAsUser = Some 1000
        , capabilities = Some kubernetes.Capabilities::{
            drop = Some [ "ALL" ]
          }
        }
    , podAnnotations = [ RejectSidecarAnnotation ]
    , environment = [
        kubernetes.EnvVar::{ name = "TLS_CA_CERT", value = Some mtls.caCertPath },
        kubernetes.EnvVar::{ name = "TLS_SERVER_CERT_CHAIN", value = Some mtls.serverCertPath },
        kubernetes.EnvVar::{ name = "TLS_SERVER_KEY", value = Some mtls.serverKeyPath },
        kubernetes.EnvVar::{ name = "CASSINI_BIND_ADDR", value = Some "0.0.0.0:${Natural/show cassiniPort}" }
      ]
    , volumes = [
        kubernetes.Volume::{
        , name = cassiniServerCertificateSecret,
        , secret = Some kubernetes.SecretVolumeSource::{ secretName = Some cassiniServerCertificateSecret }
        }
      ]
    , volumeMounts = [
        kubernetes.VolumeMount::{
          name = cassiniServerCertificateSecret,
          mountPath = tlsPath,
          readOnly = Some True
        }
      ]
    }
}
