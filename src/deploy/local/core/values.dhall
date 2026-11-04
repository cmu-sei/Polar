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

{-
  local/core/values.dhall — the core, on a local cluster.

  Everything that differs between clusters is here; the library's core (types/core.dhall) has no
  address, tag or name of its own. No secret is here: the core makes the ones it needs, with random
  values that are never printed (see plan.dhall's stages).

  The images are the fork's own, loaded into the node by hand, so they are not pulled.
-}
let Polar = ../../types/package.dhall

let values
    : Polar.core.CoreValues.Type
    = { images =
        { operator = "polar-operator:latest"
        , certIssuer = "cert-issuer:latest"
        , polarInit = "polar-init:latest"
        , cassini = "cassini:latest"
        , credentialsAgent = "credentials-agent-consumer:latest"
        , credentialsAgentObserver = "credentials-agent-observer:latest"
        , neo4j = "neo4j:5.26.2"
        , gitServer = "polar-git-server:latest"
        , schedulerProcessor = "scheduler-processor:latest"
        , schedulerObserver = "scheduler-observer:latest"
        }
      , imagePullPolicy = Some "IfNotPresent"
      , storageClass = None Text
      -- Every pod's mTLS key is the custody daemon's; the plan makes the cluster's CA once, and the issuer loads it.
      , custodyImage = Some "polar-custody:latest"
      , pinnedCa = Some "polar-ca"
      , caSecret = Some "cert-issuer-ca"
      , gitServerAdmins = [] : List Text
      -- Cassini enforces topics from the certificate: without these documents no agent may publish. They are generated
      -- from the agents' scaffolds by the consumer (Blitz: infra/tools/authority.nu).
      , authorityModel = None Text
      , issuanceFloor = None Text
      , servedWorkloads = None Text
      }

in  values
