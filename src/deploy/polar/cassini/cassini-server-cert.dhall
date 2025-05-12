let values = ../values.dhall 

let ServerCertificate = { apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = values.cassini.tls.certificateRequestName , namespace = values.namespace }
, spec = values.cassini.tls.certificateSpec

}

in ServerCertificate