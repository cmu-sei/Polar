let values = ../values.dhall 


let ClientCertificate = 
{ apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = values.gitlab.tls.certificateRequestName, namespace = values.namespace }
, spec = values.gitlab.tls.certificateSpec
}

in ClientCertificate