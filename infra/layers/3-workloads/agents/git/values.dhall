-- infra/layers/3-workloads/agents/git/values.dhall
--
-- Canonical defaults for the git agent chart.
-- Two containers here: consumer (processes commits) and scheduler (dispatches ad-hoc repo
-- observation tasks). The observer is a catalog workload: config/workloads/git.nuon.
--
-- git.json config is read from targets/<target>/conf/git.json by render.nu
-- and injected as a Secret at render time.

let Constants = ../../../../schema/constants.dhall

in  { name             = "git-agents"
    , imagePullPolicy  = "IfNotPresent"
    , imagePullSecrets = [] : List { name : Optional Text }

    , consumer =
      { name  = "git-repo-consumer"
      , image = "polar-git-consumer:latest"
      }

    , scheduler =
      { name  = "git-scheduler"
      , image = "polar-git-scheduler:latest"
      }

    , proxyCACert = None Text
    }
