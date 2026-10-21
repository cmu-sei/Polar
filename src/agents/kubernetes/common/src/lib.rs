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

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Debug;

pub mod flux;
// TODO: Update names to follow a better format - kubernetes.<CLUSTER_NAME>.<ROLE>.<RESOURCE>
// clustername should come from configuration in the future
// ideally we could name consumers and observers exactly after the resource they're supposed to look for, but the strings aren't known yet
// It looks like the API sets types to things like PodList, for example, if this is open information, we can go off of the latest api!
// Also, make sure we have names for each reasource type, KUBE_POD_OBSERVER, KUBE_NODE_OBSERVER etc
pub const KUBERNETES_CONSUMER: &str = "kubernetes.cluster.consumer.Pod";
pub const BATCH_PROCESS_ACTION: &str = "BatchProcess";
pub const RESOURCE_APPLIED_ACTION: &str = "Applied";
pub const RESOURCE_DELETED_ACTION: &str = "Delete";
pub const RESYNC_ACTIUON: &str = "Resync";
pub const KIND_OCI_REPOSITORY: &str = "OCIRepository";
pub const KIND_KUSTOMIZATION: &str = "Kustomization";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RawKubeEvent {
    /// e.g. "Pod", "ConfigMap", "Service"
    pub kind: String,
    /// e.g. "Applied", "Deleted", "InitApply"
    pub action: String,
    /// the raw JSON of the object
    pub object: Value,

    pub resource_version: Option<String>,
}

/// Messages intended to be serialized and set across the broker boundary to consumers for processing
/// Generics make things 10x simpler here, as all types from the k8s_openapi crate can be serialized with serde.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KubeMessage {
    /// Observer read a batch of resources during initial sync with the server.
    ResourceBatch { kind: String, resources: Value },
    /// A new or updated resource has been observed
    ///
    ResourceApplied { kind: String, resource: Value },

    /// A resource has been deleted
    ResourceDeleted { kind: String, resource: Value },

    /// Indicates that the watcher is resetting and will re-list resources
    ResyncStarted { kind: String, resource: Value },
    // /// A resource has been observed during re-sync (initial apply)
    // ResyncResource {
    //     kind: String
    //     resource: Value,
    // },

    // /// Resync has completed and all InitApply resources have been listed
    // ResyncCompleted {
    //     namespace: Option<String>,
    //     resource_kind: String,
    // },
}
