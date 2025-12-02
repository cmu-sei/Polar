let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall

in kubernetes.ServiceAccount::{ apiVersion = "v1"
, kind = "ServiceAccount"
, metadata = kubernetes.ObjectMeta::{ name = Some values.kubeAgent.observer.serviceAccountName, namespace = Some values.namespace }
-- We don't want the token going in every container, so we'll mount it later
, automountServiceAccountToken = Some False
}