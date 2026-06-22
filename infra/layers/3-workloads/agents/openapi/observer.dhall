-- infra/layers/3-workloads/agents/openapi/observer.dhall
--
-- OpenAPI observer deployment.
-- Fetches OpenAPI specs from a configured endpoint and publishes to Cassini.

-- openapi/observer.dhall
let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

let render =
      \(v : { name : Text, image : Text, imagePullPolicy : Text, imagePullSecrets : List { name : Optional Text }, certClientImage : Text, certIssuerUrl : Text, saTokenAudience : Text, openapiEndpoint : Text }) ->
        let volumes = [ Constants.certEmptyDirVolume, Constants.saTokenVolume v.saTokenAudience ]
        let env = Constants.commonClientEnv # [ kubernetes.EnvVar::{ name = "OPENAPI_ENDPOINT", value = Some v.openapiEndpoint } ]
        let mounts = [ Constants.certVolumeMount ]
        in  kubernetes.Deployment::{
            , metadata = kubernetes.ObjectMeta::{ name = Some v.name, namespace = Some Constants.PolarNamespace, annotations = Some [ Constants.RejectSidecarAnnotation ] }
            , spec = Some kubernetes.DeploymentSpec::{ , selector = kubernetes.LabelSelector::{ matchLabels = Some (toMap { name = v.name }) }, replicas = Some 1
              , template = kubernetes.PodTemplateSpec::{ , metadata = Some kubernetes.ObjectMeta::{ name = Some v.name, labels = Some [ { mapKey = "name", mapValue = v.name } ] }
                , spec = Some kubernetes.PodSpec::{ , imagePullSecrets = Some v.imagePullSecrets, volumes = Some volumes
                  , initContainers = Some [ functions.makeCertClientInitContainer v.certIssuerUrl v.certClientImage v.saTokenAudience ]
                  , containers = [ kubernetes.Container::{ name = v.name, image = Some v.image, imagePullPolicy = Some v.imagePullPolicy, securityContext = Some Constants.DropAllCapSecurityContext, env = Some env, volumeMounts = Some mounts } ] } } } }
in render
