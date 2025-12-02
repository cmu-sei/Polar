
let Certificate = ./types/certificate.dhall

in  \(name : Text) ->
    \(namespace: Text) ->
    \(commonName : Text) ->
    \(secretName : Text) ->
    \(dnsNames : List Text) ->
      Certificate::{
        apiVersion = "cert-manager.io/v1"
      , kind = "Certificate"
      , metadata = { name = name, namespace = namespace }
      , spec =
          { commonName = commonName
          , dnsNames = dnsNames
          , duration = "2160h"
          , issuerRef = { kind = "Issuer", name = "${commonName}-leaf-issuer" }
          , renewBefore = "360h"
          , secretName = secretName
          }
      }
