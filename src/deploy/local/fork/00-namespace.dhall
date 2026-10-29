{- The namespace the platform lives in. Fixed by the fork and by the library's constants. -}
let kubernetes = ../../types/kubernetes.dhall

let values = ../values-fork.dhall

in  [ kubernetes.Resource.Namespace
        kubernetes.Namespace::{
        , metadata = kubernetes.ObjectMeta::{ name = Some values.namespace }
        }
    ]
