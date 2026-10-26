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

--TODO: move this import somewhere better, probably under a better organized `conf` or something similar. It's primarily used by everything within src/agents
let Lib =
      -- Pinned, with its hash, to the commit flake.lock names for nix-container-lib, so the Dhall and the Nix sides of the
      -- library are one version (a moving `main` let them differ). Update both together (nix-container-lib's
      -- DEVELOPER_GUIDE.md, "Pin to a New Revision").
      https://raw.githubusercontent.com/daveman1010221/nix-container-lib/7db6e5d52c28f7bc487c549d204c7a37ce10d431/dhall/prelude.dhall
        sha256:e88446f08eba97c5e8540743a4c0458d3bee5d7debd8e072f9b9ace610abf729

in Lib
