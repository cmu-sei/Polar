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

-- dhall-k8s import for the project: 1.35, whose containers take restart rules (docs/architecture/deployments.md,
-- step 3). The three schema files that import this one pin it by the same hash, and Dhall loads a pinned import from its
-- cache by the hash: a new version here moves those pins with it, or they keep loading the old one.
let kubernetes =
      https://raw.githubusercontent.com/dhall-lang/dhall-kubernetes/refs/heads/master/1.35/package.dhall
        sha256:2086df2178fdb83a08bd4ca89c951f12e4ce16d3cc1441005fcda943a028bb8c

in  kubernetes
