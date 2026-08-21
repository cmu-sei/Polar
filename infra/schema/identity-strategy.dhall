-- infra/schema/identity-strategy.dhall
--
-- Mirrors cert_issuer_common::identity::IdentityNormalization exactly
-- (#[serde(tag = "strategy", rename_all = "snake_case")]). Shared so
-- every place that needs to construct or render one of these values
-- (currently cert-issuer's own configmap.dhall, and any target's
-- overrides.dhall) uses the identical type, rather than each
-- repeating the full union syntax inline.
< KubernetesServiceAccount : { clusterDomain : Text }
| SuffixedClaim
>
