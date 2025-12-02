let values = ../values.dhall
let Constants = ../../types/constants.dhall

let ServerCertificate = { apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = values.cassini.tls.certificateRequestName , namespace = Constants.PolarNamespace }
, spec = values.cassini.tls.certificateSpec
}

in ServerCertificate
