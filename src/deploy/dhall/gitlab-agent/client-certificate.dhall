let values = ../values.dhall 


let ClientCertificate = 
{ apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = values.mtls.gitlab.secretName, namespace = values.namespace }
, spec = values.gitlab.mtls.certificateSpec
}

in ClientCertificate