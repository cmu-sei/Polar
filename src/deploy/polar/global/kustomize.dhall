let values = ../values.dhall

let kustomize = { apiVersion = "kustomize.toolkit.fluxcd.io/v1"
, kind = "Kustomization"
, metadata = { name = "polar", namespace = values.namespace }
, resources = [
  -- TODO: flux doesn't look in other directories for manifests,
  -- let's add the directories explictitly just to be sure.
  -- We might want to dynamically build this list down the line.
  , "git-repo-secret.yaml"
  , "../neo4j"
  , "../gitlab-agent"
  
]
, spec =
  { decryption = { provider = "sops" }
  , dependsOn = [ { name = "environment" } ]
  , interval = "5m"
  , path = "./manifests/global"
  , prune = True
  , sourceRef = { kind = "GitRepository", name = values.deployRepository.name }
  }
}

in kustomize