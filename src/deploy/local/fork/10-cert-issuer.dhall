{-
  The certificate issuer. Everything else waits for it: every pod's init container asks it, directly
  or through the credentials agent, for a certificate. It makes its own CA on its claim the first
  time it starts, and trusts the cluster's service-account tokens for audience polar-cert-issuer.local.
  The issuance floor is off.
-}
let Polar = ../../types/package.dhall

let values = ../values-fork.dhall

in  Polar.platform.makeCertIssuer
      Polar.platform.CertIssuer::{
      , namespace = values.namespace
      , image = values.images.certIssuer
      , imagePullPolicy = values.imagePullPolicy
      , storageClass = values.storageClass
      -- The one identity allowed to ask it to sign on another's behalf. The fork fixes this name.
      , internalCaller = "credentials-agent.${values.namespace}.serviceaccount.cluster.local"
      }
