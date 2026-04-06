let Constants = ../../types/constants.dhall

let values = ../values-active.dhall

let ServerCertificate = { apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = "cassini-server-certificate" , namespace = Constants.PolarNamespace }
, spec = values.cassini.tls.certificateSpec
}

in ServerCertificate
