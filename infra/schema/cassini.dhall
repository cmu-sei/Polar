-- ACE Tool
--
-- Copyright 2026 Carnegie Mellon University.
--
-- NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
-- FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
-- EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
-- PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
-- MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
-- TRADEMARK, OR COPYRIGHT INFRINGEMENT.
--
-- Licensed under a MIT (SEI)-style license, please see license.txt or contact
-- permission@sei.cmu.edu for full terms.
--
-- [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
-- distribution.  Please see Copyright notice for non-US Government use and distribution.
--
-- This Software includes and/or makes use of Third-Party Software each subject to its own license.
--
-- DM26-0670

-- types/Cassini.dhall
{-
  This describes the full configuration needed to deploy an instance of Cassini
  Further configurations specific to the deployment and pod can be provided in the manifest
  TODO: Specify some reasonable constants for things like image, ports, tls stuff
-}

let kubernetes = ./kubernetes.dhall

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
