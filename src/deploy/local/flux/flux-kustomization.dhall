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

-- flux-kustomization.dhall
--
-- Three Flux Kustomizations encoding Polar's deployment order:
--
--   1. polar-core        — cert-issuer only. Nothing works without it.
--   2. polar-infra       — cassini and neo4j. Depends on polar-core.
--   3. polar-agents      — all agents. Depends on polar-infra.
--
-- Each Kustomization points at a subdirectory inside the OCI artifact,
-- so your artifact must be structured as:
--
--   core/      kustomization.yaml → cert-issuer.yaml
--   infra/     kustomization.yaml → cassini.yaml, neo4j.yaml
--   agents/    kustomization.yaml → agents.yaml (and jaeger, storage, etc.)
--
-- Generate and apply:
--   dhall-to-yaml --documents <<< ./flux-kustomization.dhall | kubectl apply -f -
--
-- Watch the chain:
--   kubectl get kustomization -n flux-system -w
--   flux logs --all-namespaces --follow

let mkKustomization =
      \(name : Text) ->
      \(path : Text) ->
      \(dependsOn : List { name : Text }) ->
        { apiVersion = "kustomize.toolkit.fluxcd.io/v1"
        , kind       = "Kustomization"
        , metadata   = { name, namespace = "flux-system" }
        , spec =
          { interval  = "1m"
          , sourceRef = { kind = "OCIRepository", name = "polar-test" }
          , path
          , prune     = True
          , timeout   = "2m"
          , dependsOn
          }
        }

in  [ mkKustomization "polar-core"   "./core"   ([] : List { name : Text })
    , mkKustomization "polar-infra"  "./infra"  [ { name = "polar-core" } ]
    , mkKustomization "polar-agents" "./agents" [ { name = "polar-infra" } ]
    ]
