
let Certificate = ./types/certificate.dhall

let GetCertificate = \(name : Text) ->
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
-- helper to create a registry secret provided a name, namespace, and base64 encoded config.json
let DockerRegistrySecret =
          \(secretName : Text) ->
          \(namespace : Text) ->
          \(dockerconfig_b64 : Text) ->
            Secret::{
            , apiVersion = "v1"
            , kind = "Secret"
            , metadata = ObjectMeta::{
                , name = Some secretName
                , namespace = Some namespace
              }
            , type = Some "kubernetes.io/dockerconfigjson"
            , data = Some
                [ { mapKey = ".dockerconfigjson"
                  , mapValue = dockerconfig_b64
                  }
                ]
            }

in
{GetCertificate, ImagePullSecret }
