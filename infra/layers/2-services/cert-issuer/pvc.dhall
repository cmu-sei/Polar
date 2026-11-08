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

-- infra/layers/2-services/cert-issuer/pvc.dhall
-- PVC for CA materials. Must survive pod restarts — emptyDir is not acceptable.

let Constants = ../../../schema/constants.dhall

let render =
      \(v : { name : Text, caVolumeName : Text, caStorageClass : Text, caStorageSize : Text }) ->
        { apiVersion = "v1"
        , kind       = "PersistentVolumeClaim"
        , metadata   =
          { name      = "${v.name}-ca"
          , namespace = Constants.PolarNamespace
          }
        , spec =
          { accessModes      = [ "ReadWriteOnce" ]
          , storageClassName = v.caStorageClass
          , resources.requests.storage = v.caStorageSize
          }
        }

in render
