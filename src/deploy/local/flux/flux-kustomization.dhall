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
