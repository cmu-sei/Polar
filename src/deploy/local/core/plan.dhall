{-
  local/core/plan.dhall — the core of a Polar deployment, by itself.

    polar plan local/core/plan.dhall
    polar up   local/core/plan.dhall --context <kube-context>

  Applications do not need this: an application's plan includes the core, so one plan stands up an
  application with everything it needs (see Blitz's infra/plan.dhall). This one is for standing up the
  core alone, or for looking at it.
-}
let Polar = ../../types/package.dhall

in  { name = "polar-core", stages = (Polar.core.core ./values.dhall).stages }
