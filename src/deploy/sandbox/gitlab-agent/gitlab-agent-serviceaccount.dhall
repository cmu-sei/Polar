let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall

let serviceAccount 
    = kubernetes.ServiceAccount::{ 
        apiVersion = "v1"
        , kind = "ServiceAccount"
        , metadata = kubernetes.ObjectMeta:: { name = Some values.gitlab.serviceAccountName, namespace = Some values.namespace }
    }

in serviceAccount