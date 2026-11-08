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

-- infra/layers/1-platform/infra/namespaces/namespaces.dhall
--
-- Defines all namespaces required by the Polar stack.
-- Applied first -- everything else depends on these existing.

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall

in  [ kubernetes.Resource.Namespace
        kubernetes.Namespace::{
        , apiVersion = "v1"
        , kind       = "Namespace"
        , metadata   = kubernetes.ObjectMeta::{
          , name = Some Constants.PolarNamespace
          }
        }
    , kubernetes.Resource.Namespace
        kubernetes.Namespace::{
        , apiVersion = "v1"
        , kind       = "Namespace"
        , metadata   = kubernetes.ObjectMeta::{
          , name = Some Constants.GraphNamespace
          }
        }
    , kubernetes.Resource.Namespace
        kubernetes.Namespace::{
        , apiVersion = "v1"
        , kind       = "Namespace"
        , metadata   = kubernetes.ObjectMeta::{
          , name = Some Constants.AuthNamespace
          }
        }
    -- Holds exactly one thing: the ServiceAccount whose token proves
    -- cluster control to the credentials agent at bootstrap. Nothing runs
    -- here, and nothing is bound to that ServiceAccount. Kept apart from
    -- every workload namespace so no pod can ever be granted its token by
    -- accident.
    , kubernetes.Resource.Namespace
        kubernetes.Namespace::{
        , apiVersion = "v1"
        , kind       = "Namespace"
        , metadata   = kubernetes.ObjectMeta::{
          , name = Some "polar-bootstrap"
          }
        }
    -- The identity whose token proves cluster control at bootstrap. No
    -- Role, ClusterRole or binding may ever reference it: its only power is
    -- that cert-issuer's TokenReview accepts its token, and only
    -- the credentials agent's allowlist
    -- (system:serviceaccount:polar-bootstrap:polar-admin-bootstrap)
    -- turns that into the bootstrap claim. Tokens are minted on demand
    -- by the operator's own kubeconfig; nothing mounts one.
    , kubernetes.Resource.ServiceAccount
        kubernetes.ServiceAccount::{
        , apiVersion = "v1"
        , kind       = "ServiceAccount"
        , metadata   = kubernetes.ObjectMeta::{
          , name      = Some "polar-admin-bootstrap"
          , namespace = Some "polar-bootstrap"
          }
        , automountServiceAccountToken = Some False
        }
    ]
