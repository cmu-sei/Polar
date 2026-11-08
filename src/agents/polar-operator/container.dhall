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

-- src/agents/polar-operator/container.dhall
--
-- The operator's image: the binary and the CA bundle it needs to reach the API server. Pre-rendered
-- to container.nix with `dhall-to-nix < container.dhall > container.nix` (see `just` recipe
-- `update-container-lib`), because Nix cannot fetch the library inside its sandbox.

let Lib = ../../containers/container-lib.dhall

let defaults = Lib.defaults

in  defaults.minimalContainer //
      { name       = "polar-operator"
      , entrypoint = Some "polar-operator"
      , staticUid  = Some 65532
      , staticGid  = Some 65532
      , extraEnv =
          [ Lib.buildEnv "SSL_CERT_FILE" "/etc/ssl/certs/ca-bundle.crt"
          , Lib.buildEnv "SSL_CERT_DIR"  "/etc/ssl/certs"
          ]
      }
