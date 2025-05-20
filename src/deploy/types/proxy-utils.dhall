-- Some re-useable functions to append relevant data depending on whether a path to a proxy certificate was provided.
let kubernetes = ./kubernetes.dhall

let ProxyVolume =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(certName : Text) →
                [ kubernetes.Volume::{
                    , name = certName
                    , secret = Some kubernetes.SecretVolumeSource::{
                        secretName = Some certName
                    }
                  }
                ]
          , None = [] : List kubernetes.Volume.Type
          }
          cert

let ProxyMount =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(cert : Text) →
                [ kubernetes.VolumeMount::{
                    , name = cert
                    , mountPath = "/etc/tls/proxy"
                    , readOnly = Some True
                  }
                ]
          , None = [] : List kubernetes.VolumeMount.Type
          }
          cert

-- Adds an environment variable to point to a path where a proxy CA certificate will be mounted
let ProxyEnv =
      λ(cert : Optional Text) →
        merge
          { Some =
              λ(_ : Text) →
                [ kubernetes.EnvVar::{
                    , name = "PROXY_CA_CERT"
                    , value = Some "/etc/tls/proxy/proxy.crt"
                  }
                ]
          , None = [] : List kubernetes.EnvVar.Type
          }
          cert
in  { ProxyVolume, ProxyMount, ProxyEnv }