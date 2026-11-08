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

-- infra/schema/resources.dhall
--
-- The objects that live alongside a workload: for each cluster role, the
-- ClusterRole and a ClusterRoleBinding of the same name to the workload's
-- ServiceAccount; for each token Secret, a service-account-token Secret for
-- that ServiceAccount; for each persistent volume claim and Service, the object itself.
--
-- The ServiceAccount itself is not emitted here: render.nu emits one for
-- every workload identity no manifest declares.
--
-- The token controller fills a service-account-token Secret only after its
-- ServiceAccount exists, so on a fresh cluster a first apply may need
-- repeating (tracked in INFRA.md).

let kubernetes = ./kubernetes.dhall

let Constants = ./constants.dhall

let T = ./resources-spec.dhall

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

let render =
      λ(v : T.Spec) →
        let toRule =
              λ(r : T.Rule) →
                kubernetes.PolicyRule::{
                , apiGroups = Some r.api_groups
                , resources = Some r.resources
                , verbs = r.verbs
                }

        let toClusterRole =
              λ(c : T.ClusterRoleEntry) →
                kubernetes.Resource.ClusterRole
                  kubernetes.ClusterRole::{
                  , apiVersion = "rbac.authorization.k8s.io/v1"
                  , kind = "ClusterRole"
                  , metadata = kubernetes.ObjectMeta::{ name = Some c.name }
                  , rules = Some
                      (Prelude.List.map T.Rule kubernetes.PolicyRule.Type toRule c.rules)
                  }

        let toBinding =
              λ(c : T.ClusterRoleEntry) →
                kubernetes.Resource.RoleBinding
                  kubernetes.RoleBinding::{
                  , apiVersion = "rbac.authorization.k8s.io/v1"
                  , kind = "ClusterRoleBinding"
                  , metadata = kubernetes.ObjectMeta::{ name = Some c.name }
                  , roleRef = kubernetes.RoleRef::{
                    , apiGroup = ""
                    , kind = "ClusterRole"
                    , name = c.name
                    }
                  , subjects = Some
                    [ kubernetes.Subject::{
                      , kind = "ServiceAccount"
                      , name = v.service_account
                      , namespace = Some v.namespace
                      }
                    ]
                  }

        let toTokenSecret =
              λ(secretName : Text) →
                kubernetes.Resource.Secret
                  kubernetes.Secret::{
                  , apiVersion = "v1"
                  , kind = "Secret"
                  , metadata = kubernetes.ObjectMeta::{
                    , name = Some secretName
                    , namespace = Some v.namespace
                    , annotations = Some
                      [ { mapKey = "kubernetes.io/service-account.name"
                        , mapValue = v.service_account
                        }
                      ]
                    }
                  , type = Some "kubernetes.io/service-account-token"
                  }

        let toConfigMap =
              λ(c : T.ConfigMapEntry) →
                kubernetes.Resource.ConfigMap
                  kubernetes.ConfigMap::{
                  , apiVersion = "v1"
                  , kind = "ConfigMap"
                  , metadata = kubernetes.ObjectMeta::{
                    , name = Some c.name
                    , namespace = Some v.namespace
                    }
                  , data = Some [ { mapKey = c.key, mapValue = c.content } ]
                  }

        let toPvc =
              λ(c : T.PvcEntry) →
                kubernetes.Resource.PersistentVolumeClaim
                  kubernetes.PersistentVolumeClaim::{
                  , metadata = kubernetes.ObjectMeta::{
                    , name = Some c.name
                    , namespace = Some v.namespace
                    }
                  , spec = Some kubernetes.PersistentVolumeClaimSpec::{
                    , accessModes = Some [ "ReadWriteOnce" ]
                    , storageClassName = Some c.storage_class
                    , resources = Some kubernetes.VolumeResourceRequirements::{
                      , requests = Some [ { mapKey = "storage", mapValue = c.size } ]
                      }
                    }
                  }

        let toService =
              λ(c : T.ServiceEntry) →
                kubernetes.Resource.Service
                  kubernetes.Service::{
                  , metadata = kubernetes.ObjectMeta::{
                    , name = Some c.name
                    , namespace = Some v.namespace
                    }
                  , spec = Some kubernetes.ServiceSpec::{
                    , selector = Some [ { mapKey = "name", mapValue = c.selector } ]
                    , ports = Some
                        ( Prelude.List.map
                            T.ServicePort
                            kubernetes.ServicePort.Type
                            ( λ(p : T.ServicePort) →
                                kubernetes.ServicePort::{
                                , name = p.name
                                , port = p.port
                                , targetPort = Some (kubernetes.NatOrString.Nat p.port)
                                }
                            )
                            c.ports
                        )
                    }
                  }

        in    Prelude.List.map T.ClusterRoleEntry kubernetes.Resource toClusterRole v.cluster_roles
            # Prelude.List.map T.ClusterRoleEntry kubernetes.Resource toBinding v.cluster_roles
            # Prelude.List.map Text kubernetes.Resource toTokenSecret v.sa_token_secrets
            # Prelude.List.map T.ConfigMapEntry kubernetes.Resource toConfigMap v.config_maps
            # Prelude.List.map T.PvcEntry kubernetes.Resource toPvc v.pvcs
            # Prelude.List.map T.ServiceEntry kubernetes.Resource toService v.services

in  render
