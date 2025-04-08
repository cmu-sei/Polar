let values = ../values.dhall 

let ServerCertificate = { apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = values.mtls.cassini.secretName , namespace = values.namespace }
, spec = values.cassini.mtls.certificateSpec

}

in ServerCertificate