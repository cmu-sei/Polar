let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let values = ../values.dhall

let serviceAccount 
    = kubernetes.ServiceAccount::{ 
        apiVersion = "v1"
        , kind = "ServiceAccount"
        , metadata = kubernetes.ObjectMeta:: { name = Some values.gitlab.serviceAccountName, namespace = Some values.namespace }
    }

in serviceAccount