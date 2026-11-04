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
  examples/builder-companions.dhall — what goes beside builder-agent.dhall: the identity the agent
  runs as, the Service that fronts it and the claim that keeps its state.

  Render with:  dhall-to-yaml --documents --file builder-companions.dhall > builder-companions.yaml
-}
let Polar = ../types/package.dhall

let kubernetes = ../types/kubernetes.dhall

let B = Polar.builder

in  [ kubernetes.Resource.ServiceAccount (B.makeServiceAccount "example" "example")
    , kubernetes.Resource.Service (B.makeService "example" "example" 8080)
    , kubernetes.Resource.PersistentVolumeClaim
        (B.makePvc "example" "example-data" "1Gi" (None Text))
    ]
