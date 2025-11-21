{ apiVersion = "cert-manager.io/v1"
, kind = "Issuer"
, metadata = { name = "polar-leaf-issuer", namespace = "polar" }
, spec.ca.secretName = "ca-cert"
}
