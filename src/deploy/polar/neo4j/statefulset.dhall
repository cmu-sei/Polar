let kubernetes = ../../types/kubernetes.dhall
let values = ../values.dhall

let configVolume = "neo4j-config-copy"
let certificatesVolume = "neo4j-certificates"

let confDir = "/var/lib/neo4j/conf"
let certDir = "/var/lib/neo4j/certificates"

-- TODO: Refine as we go
let setupScript =
      ''
      #!/bin/sh
      NEO4J_HOME=/var/lib/neo4j

      set -e

      echo "[INIT] Copying neo4j.conf..."
      cp /config/neo4j.conf $NEO4J_HOME/conf/neo4j.conf
      
      # echo "[INIT] Copying certificates..."

      # mkdir -p $NEO4J_HOME/certificates/https/trusted
      # mkdir -p $NEO4J_HOME/certificates/bolt/trusted 
      
      # cp /secrets/tls.key $NEO4J_HOME/certificates/https/tls.key
      # cp /secrets/tls.crt $NEO4J_HOME/certificates/https/tls.crt
      # cp /secrets/tls.key $NEO4J_HOME/certificates/bolt/tls.key
      # cp /secrets/tls.crt $NEO4J_HOME/certificates/bolt/tls.crt
      # cp /secrets/tls.crt $NEO4J_HOME/certificates/https/trusted/tls.crt
      # cp /secrets/tls.crt $NEO4J_HOME/certificates/bolt/trusted/tls.crt
      ''

let spec 
  = kubernetes.PodSpec::{
    , securityContext = Some values.neo4j.podSecurityContext
    , imagePullSecrets = Some values.sandboxRegistry.imagePullSecrets
    , initContainers = Some [
      kubernetes.Container::{
        name = "neo4j-init"
        , image = Some "${values.sandboxRegistry.url}/alpine:3.14.0"
        -- TOOD: Update this to run a script that will also chown certificate files to be owned by the security context user
        , command = Some [ "/bin/sh", "-c" ]
        , args = Some [ setupScript ]
        , securityContext = Some values.neo4j.containerSecurityContext
        , volumeMounts = Some [
          -- mount the read-only config
          , kubernetes.VolumeMount::{
              name  = values.neo4j.config.name
              , mountPath = "/config"
            }
          -- mount the certificates 
          -- TODO: Reenable if we go back to using ssl
          -- , kubernetes.VolumeMount::{
          --     name  = values.neo4j.tls.leafSecretName
          --     , mountPath = "/secrets"
          --   } 
            -- Mount empty conf dir to copy into
          , kubernetes.VolumeMount::{
            name  = configVolume
            , mountPath = "/var/lib/neo4j/conf"
          } 
          -- Mount empty certificate dir to copy into
          -- TODO: Reenable if we go back to using ssl
          -- , kubernetes.VolumeMount::{
          --   name  = certificatesVolume
          --   , mountPath = "/var/lib/neo4j/certificates"
          -- } 
        ]
      }
    ]
    , containers =
      [ 
        kubernetes.Container::{
        , name = values.neo4j.name
        , image = Some values.neo4j.image
        -- TODO: Activate based on some debug flag
        -- , command = Some ["sleep"]
        -- , args = Some ["infinity"]
        , env = Some values.neo4j.env
        , securityContext = Some values.neo4j.containerSecurityContext
        , ports = Some values.neo4j.containerPorts
        , volumeMounts = Some [
            -- mount writeable config
            ,kubernetes.VolumeMount::{
                name  = configVolume
                , mountPath = "/var/lib/neo4j/conf"
            }
            -- Mount the server certificates
            -- TODO: Reenable if we go back to using SSL
            -- ,kubernetes.VolumeMount::{
            --     name  = certificatesVolume
            --     , mountPath = "/var/lib/neo4j/certificates"
            --     , readOnly = Some True
            -- }           
            -- Mount PVCs for data, logs, etc.
            ,kubernetes.VolumeMount::{
                name  = values.neo4j.volumes.data.name
                , mountPath = values.neo4j.volumes.data.mountPath
            }
            ,kubernetes.VolumeMount::{
                name  = values.neo4j.volumes.logs.name
                , mountPath = values.neo4j.volumes.logs.mountPath
            }
        ]
        },
      ]
    , volumes = Some [
        , kubernetes.Volume::{
            , name = values.neo4j.volumes.data.name
            , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                claimName = values.neo4j.volumes.data.name
            }
        }
        , kubernetes.Volume::{
            , name = values.neo4j.volumes.logs.name
            , persistentVolumeClaim = Some kubernetes.PersistentVolumeClaimVolumeSource::{
                claimName = values.neo4j.volumes.logs.name
            }
        }
        -- Define a volume containing the certificate chain and the private key for the server
        -- TODO: Reenable if we go back to using SSL
        -- , kubernetes.Volume::{
        --     , name = values.neo4j.tls.leafSecretName
        --     , secret = Some kubernetes.SecretVolumeSource::{
        --         secretName = Some values.neo4j.tls.leafSecretName
        --         , items = Some [ { key = "tls.crt", path = "tls.crt" , mode = None Natural }, { key = "tls.key", path = "tls.key" , mode = None Natural } ] 

        --     }
        -- }
        -- A volume for our initial read-only neo4j configmap 
        , kubernetes.Volume::{
          , name = values.neo4j.config.name 
          , configMap = Some kubernetes.ConfigMapVolumeSource::{
            name = Some values.neo4j.config.name
            , items = Some [ kubernetes.KeyToPath::{ key = "neo4j.conf", path = "neo4j.conf" } ]
            }
        }
        -- This will be our shared, writeable emptyDir volume for the neo4j.conf file.
        -- Firstly, neo4j requires the config to be writable, which conflicts with k8s' mandatory read-only configmaps.
        -- So we need to use the init container to copy a writable version over.
        , kubernetes.Volume::{
          name = configVolume
          , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
        }
        -- Next,
        -- Kubernetes volume mounts are brutally literal — and they do not merge overlapping mounts.
        -- If we mount something to /var/lib/neo4j/certificates/https twice, only one will survive. The same happens for /bolt.
        -- So, we add an emptyDir to set up everything under var/lib/neo4j, we'll then mount this to the neo4j container
        -- REFERENCE: https://neo4j.com/docs/operations-manual/5/security/ssl-framework/#ssl-configuration
        -- , kubernetes.Volume::{
        --   name = certificatesVolume
        --   , emptyDir = Some kubernetes.EmptyDirVolumeSource::{=}
        -- }
    ]
  }

let statefulSet = 
    kubernetes.StatefulSet::{ 
      apiVersion = "apps/v1"
    , kind = "StatefulSet"
    , metadata = kubernetes.ObjectMeta::{
        name = Some values.neo4j.name
        , namespace = Some values.neo4j.namespace
      }
    , spec = Some kubernetes.StatefulSetSpec::{ 
      selector = kubernetes.LabelSelector::{
        matchLabels = Some (toMap { name = values.neo4j.name })
      }
      , serviceName = values.neo4j.service.name
      , template = kubernetes.PodTemplateSpec::{
          , metadata = Some kubernetes.ObjectMeta::{
                name = Some values.neo4j.name
            ,   labels = Some [ { mapKey = "name", mapValue = values.neo4j.name } ]
            -- , annotations = Some values.neo4j.podAnnotations
            }
          , spec = Some spec 
      }
      , replicas = Some 1
    }
  }

in  statefulSet