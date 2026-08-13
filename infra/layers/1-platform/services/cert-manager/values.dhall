-- infra/layers/1-platform/services/cert-manager/values.dhall
--
-- Canonical defaults for the cert-manager chart.
-- Currently all cert-manager configuration derives directly from
-- schema/constants.dhall (issuer names, cert names, namespaces).
-- This file exists as the override target for targets that need
-- to diverge from those defaults.

{=}
