let values = ../values.dhall
let kubernetes = ../../types/kubernetes.dhall

-- define a token for our agent's service account
in kubernetes.Secret::{ apiVersion = "v1"
, kind = "Secret"
, metadata 
  = kubernetes.ObjectMeta::{ 
    name = Some values.kubeAgent.observer.secretName
    , namespace = Some values.namespace 
    , annotations = Some [
      {
        , mapKey = "kubernetes.io/service-account.name"
        , mapValue = values.kubeAgent.observer.serviceAccountName
      }
    ]
  }
  , type = Some "kubernetes.io/service-account-token"
}
