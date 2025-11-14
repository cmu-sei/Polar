{ apiVersion = "cert-manager.io/v1"
, kind = "Certificate"
, metadata = { name = "ca-cert", namespace = "polar" }
, spec =
  { commonName = "polar"
  , isCA = True
  , issuerRef = { kind = "Issuer", name = "ca-issuer" }
  , secretName = "ca-cert"
  }
}
