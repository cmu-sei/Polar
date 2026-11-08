/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! Object key conventions for build-orchestrator artifact storage.
//!
//! All objects live under a build-scoped prefix so any build's artifacts
//! can be listed, fetched, or deleted as a unit:
//!
//!   builds/<build_id>/pipeline.log    — stdout+stderr from the pipeline container
//!   builds/<build_id>/clone.log       — stdout+stderr from the clone init container
//!   builds/<build_id>/manifest.json   — the k8s Job spec submitted for this build
//!
//! The build_id is a UUID, which makes keys globally unique and directly
//! correlatable with BuildRecord entries in the registry actor and with
//! Polar's knowledge graph once the Cassini integration is wired up.

use uuid::Uuid;

pub fn build_prefix(build_id: Uuid) -> String {
    format!("builds/{}", build_id)
}

pub fn pipeline_log_key(build_id: Uuid) -> String {
    format!("builds/{}/pipeline.log", build_id)
}

pub fn clone_log_key(build_id: Uuid) -> String {
    format!("builds/{}/clone.log", build_id)
}

pub fn manifest_key(build_id: Uuid) -> String {
    format!("builds/{}/manifest.json", build_id)
}
