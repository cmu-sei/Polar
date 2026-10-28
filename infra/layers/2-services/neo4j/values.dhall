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

-- infra/layers/2-services/neo4j/values.dhall
--
-- Canonical defaults for the neo4j chart.
-- Targets override only what differs via targets/<target>/overrides.dhall.
--
-- Secrets (NEO4J_AUTH) are not here — they are threaded in by render.nu
-- at render time. TLS certs are now issued by the cert-issuer at pod
-- startup via the polar-init init container.

{ name               = "polar-neo4j"
-- Its node pool, where a target declares placement: beside the graph writers, its only writers, and off the observers'
-- node (docs/architecture/placement.md; the maintainer, 2026-10-28).
, pool               = "writers"
-- The ServiceAccount, and so the certificate identity
-- (<sa>.<ns>.serviceaccount.cluster.local). Deliberately not `name`: that
-- names the StatefulSet, its pod labels and its pods, which the Service
-- selector and several Justfile recipes depend on.
, serviceAccountName = "graph"
, hostName           = "neo4j"
, namespace          = "polar-graph"
, image              = "neo4j:5.26.2"
, imagePullPolicy    = "Always"
, configVolume       = "neo4j-config-copy"
, certIssuerUrl      = ""
, nuInitImage        = "polar-nu-init:latest"
, certIssuerAudience = ""
, neo4jSans          = [] : List Text
, ports =
  { http  = 7474
  , https = 7473
  , bolt  = 7687
  }
, config =
  { name = "neo4j-config"
  , path = "/var/lib/neo4j/conf"
  }
, volumes =
  { data =
    { name             = "polar-db-data"
    , storageClassName = Some "managed-csi"
    , storageSize      = "10Gi"
    , mountPath        = "/var/lib/neo4j/data"
    }
  , logs =
    { name             = "polar-db-logs"
    , storageClassName = Some "managed-csi"
    , storageSize      = "10Gi"
    , mountPath        = "/var/lib/neo4j/logs"
    }
  , certs =
    { name             = "polar-db-certs"
    , storageClassName = Some "managed-csi"
    , storageSize      = "1Gi"
    , mountPath        = "/var/lib/neo4j/certificates"
    }
  }
}
