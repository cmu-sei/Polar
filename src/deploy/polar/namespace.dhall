
let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.31/package.dhall
      sha256:1a0d599eabb9dd154957edc59bb8766ea59b4a245ae45bdd55450654c12814b0

let polarNamespace = kubernetes.Namespace::{
    metadata = kubernetes.ObjectMeta::{ name = Some "polar" }
}

in polarNamespace
