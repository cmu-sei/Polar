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

-- flux-oci-repository.dhall
--
-- Flux OCIRepository pointing at the local insecure registry.
-- Tells source-controller to poll for new digests on the given tag.
--
-- Generate and apply:
--   dhall-to-yaml <<< ./flux-oci-repository.dhall | kubectl apply -f -
--
-- Watch:
--   kubectl get ocirepository polar-test -n flux-system -w
--
-- Prerequisites:
--   Push an artifact first:
--     flux push artifact oci://localhost:31500/polar-test:latest \
--       --path=./test/manifests \
--       --source=https://github.com/cmu-sei/Polar \
--       --revision=main@sha1:$(git rev-parse HEAD)
--
--   The --revision flag sets org.opencontainers.image.revision on the artifact,
--   which Flux surfaces as status.lastAppliedOriginRevision on the Kustomization.
--   Without it that field stays null and the full chain cannot be verified.

{ apiVersion = "source.toolkit.fluxcd.io/v1"
, kind = "OCIRepository"
, metadata =
  { name = "polar-test"
  , namespace = "flux-system"
  }
, spec =
  { interval = "1m"
    -- In-cluster address of the local registry (ClusterIP service).
    -- Use localhost:31500 only from the host when pushing.
  , url = "oci://registry.local-registry.svc.cluster.local:5000/manifests/polar"
  , ref = { tag = "latest" }
    -- Required because the local registry has no TLS.
  , insecure = True
  }
}
