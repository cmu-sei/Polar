
let common = ../types.dhall

-- TODO: a deployment containing contaienrs for both the linker and resolver agent, and the necessary configuration
--
-- Volumes accessible to our gitlab agent
--let volumes =
--      [
--         TODO: add our root-ca-secret for communicating with cassini
--        kubernetes.Volume::{
--          , name = values.gitlab.tls.certificateSpec.secretName
--          , secret = Some kubernetes.SecretVolumeSource::{
--              secretName = Some values.gitlab.tls.certificateSpec.secretName
--            }
--        }
--      ]
--  # proxyUtils.ProxyVolume proxyCACert

let linkerEnv =
      common.cassiniClientEnvVars
      # common.commonNeo4jVars
      # [] -- TODO: insert any vars specific to teh agent here.

let resolverEnv =
    common.cassiniClientEnvVars
    # proxyUtils.ProxyEnv proxyCACert
    # [] -- TODO: insert any vars specific to teh agent here.

--let linkerVolumeMounts =
--      [ kubernetes.VolumeMount::{ name = values.gitlab.tls.certificateSpec.secretName, mountPath = values.tlsPath } ]
--    # proxyUtils.ProxyMount proxyCACert

in linkerEnv
