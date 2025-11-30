-- types/Cassini.dhall
{-
  This describes the full configuration needed to deploy Cassini
-}

let kubernetes = ./kubernetes.dhall
let mtls = ../path/to/mtls.dhall

let Cassini = {
 name : Text
, image : Text
, ports : { http: Natural, tcp: Natural }
-- We use cert manager handle certificate issuance, so this configuration is mostly centered
-- around configuring it and letting us pass values around.
, tls :
    { certificateRequestName : Text
    , certificateSpec :
      { commonName : Text -- Common name of the certificate issuer
      , dnsNames : List Text -- DNS names to associate with the certificate
      , duration : Text -- How long the certis should be valid for
      , issuerRef : { kind : Text, name : Text } -- Reference to the cert manager certificate issuer
      , renewBefore : Text
      , secretName : Text -- the name that should be used for the secret containing the cert/key pair
      }
    }
}

in Cassini
