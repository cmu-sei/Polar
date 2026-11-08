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

//! What the executor needs from a cluster, and nothing more.
//!
//! The executor is written against this trait, so that its order, its waits and its reports can be
//! tested with [`fake::Fake`] and no cluster. [`crate::kube_cluster::KubeCluster`] is the real one.

use crate::plan::{Id, Object};
use crate::secrets::Secret;
use anyhow::Result;
use serde_json::Value as Json;
use std::collections::BTreeMap;

/// What applying an object came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Done,
    /// The cluster does not know the object's kind. Real only in a dry run, before an earlier stage
    /// has installed the CRD that defines it.
    UnknownKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deleted {
    Removed,
    Absent,
}

#[allow(async_fn_in_trait)]
pub trait Cluster {
    /// Install the PolarAgent CustomResourceDefinition and wait until the cluster serves it.
    async fn install_crd(&self, dry_run: bool) -> Result<()>;

    /// Apply an object server-side, so that applying it again changes nothing.
    async fn apply(&self, object: &Object, dry_run: bool) -> Result<Applied>;

    async fn delete(&self, id: &Id) -> Result<Deleted>;

    /// The object as the cluster has it, or `None` if it is not there (or its kind is not known).
    async fn get(&self, id: &Id) -> Result<Option<Json>>;

    /// The keys of a Secret and what they hold, or `None` if it is not there.
    async fn secret(&self, namespace: &str, name: &str)
    -> Result<Option<BTreeMap<String, String>>>;

    async fn create_secret(
        &self,
        namespace: &str,
        name: &str,
        data: &BTreeMap<String, Secret>,
        dry_run: bool,
    ) -> Result<()>;

    async fn delete_secret(&self, namespace: &str, name: &str) -> Result<Deleted>;

    /// The keys of a ConfigMap and what they hold, or `None` if it is not there.
    async fn config_map(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>>;

    /// Make a ConfigMap hold these keys, creating it if it is absent. Applied server-side, so keys that
    /// someone else put there are left alone.
    async fn write_config_map(
        &self,
        namespace: &str,
        name: &str,
        data: &BTreeMap<String, String>,
        dry_run: bool,
    ) -> Result<()>;

    async fn delete_config_map(&self, namespace: &str, name: &str) -> Result<Deleted>;
}

#[cfg(test)]
pub mod fake {
    //! A cluster in memory that records what was asked of it, in order.

    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// Decides what `get` returns for an id on the n-th time it is asked (from 0).
    pub type Script = Box<dyn Fn(&Id, usize) -> Option<Json>>;

    #[derive(Default)]
    pub struct Fake {
        pub calls: RefCell<Vec<String>>,
        pub objects: RefCell<BTreeMap<Id, Json>>,
        pub secrets: RefCell<BTreeMap<(String, String), BTreeMap<String, String>>>,
        pub config_maps: RefCell<BTreeMap<(String, String), BTreeMap<String, String>>>,
        pub gets: RefCell<BTreeMap<Id, usize>>,
        pub script: RefCell<Option<Script>>,
        /// Kinds the cluster does not know until the CRD is installed.
        pub crd_kinds: Vec<String>,
        pub crd_installed: RefCell<bool>,
    }

    impl Fake {
        pub fn log(&self, call: String) {
            self.calls.borrow_mut().push(call);
        }

        pub fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        pub fn with_script(self, script: Script) -> Self {
            *self.script.borrow_mut() = Some(script);
            self
        }

        fn knows(&self, kind: &str) -> bool {
            !self.crd_kinds.iter().any(|k| k == kind) || *self.crd_installed.borrow()
        }
    }

    impl Cluster for Fake {
        async fn install_crd(&self, dry_run: bool) -> Result<()> {
            self.log(format!("crd{}", if dry_run { " (dry run)" } else { "" }));
            if !dry_run {
                *self.crd_installed.borrow_mut() = true;
            }
            Ok(())
        }

        async fn apply(&self, object: &Object, dry_run: bool) -> Result<Applied> {
            let id = object.id().expect("an object with an identity");
            if !self.knows(&id.kind) {
                self.log(format!("apply {id} (unknown kind)"));
                return Ok(Applied::UnknownKind);
            }
            self.log(format!(
                "apply {id}{}",
                if dry_run { " (dry run)" } else { "" }
            ));
            if !dry_run {
                self.objects.borrow_mut().insert(id, object.0.clone());
            }
            Ok(Applied::Done)
        }

        async fn delete(&self, id: &Id) -> Result<Deleted> {
            self.log(format!("delete {id}"));
            Ok(if self.objects.borrow_mut().remove(id).is_some() {
                Deleted::Removed
            } else {
                Deleted::Absent
            })
        }

        async fn get(&self, id: &Id) -> Result<Option<Json>> {
            let n = {
                let mut gets = self.gets.borrow_mut();
                let n = gets.entry(id.clone()).or_insert(0);
                *n += 1;
                *n - 1
            };
            self.log(format!("get {id}"));
            if let Some(script) = self.script.borrow().as_ref() {
                return Ok(script(id, n));
            }
            Ok(self.objects.borrow().get(id).cloned())
        }

        async fn secret(
            &self,
            namespace: &str,
            name: &str,
        ) -> Result<Option<BTreeMap<String, String>>> {
            Ok(self
                .secrets
                .borrow()
                .get(&(namespace.to_string(), name.to_string()))
                .cloned())
        }

        async fn create_secret(
            &self,
            namespace: &str,
            name: &str,
            data: &BTreeMap<String, Secret>,
            dry_run: bool,
        ) -> Result<()> {
            // The call log names the secret and its keys, as the real output does; never a value.
            self.log(format!(
                "secret {namespace}/{name} [{}]{}",
                data.keys().cloned().collect::<Vec<_>>().join(","),
                if dry_run { " (dry run)" } else { "" }
            ));
            if !dry_run {
                let held = data
                    .iter()
                    .map(|(k, v)| (k.clone(), v.expose().to_string()))
                    .collect();
                self.secrets
                    .borrow_mut()
                    .insert((namespace.to_string(), name.to_string()), held);
            }
            Ok(())
        }

        async fn delete_secret(&self, namespace: &str, name: &str) -> Result<Deleted> {
            self.log(format!("delete secret {namespace}/{name}"));
            Ok(
                if self
                    .secrets
                    .borrow_mut()
                    .remove(&(namespace.to_string(), name.to_string()))
                    .is_some()
                {
                    Deleted::Removed
                } else {
                    Deleted::Absent
                },
            )
        }

        async fn config_map(
            &self,
            namespace: &str,
            name: &str,
        ) -> Result<Option<BTreeMap<String, String>>> {
            Ok(self
                .config_maps
                .borrow()
                .get(&(namespace.to_string(), name.to_string()))
                .cloned())
        }

        async fn write_config_map(
            &self,
            namespace: &str,
            name: &str,
            data: &BTreeMap<String, String>,
            dry_run: bool,
        ) -> Result<()> {
            self.log(format!(
                "configmap {namespace}/{name} [{}]{}",
                data.keys().cloned().collect::<Vec<_>>().join(","),
                if dry_run { " (dry run)" } else { "" }
            ));
            if !dry_run {
                self.config_maps
                    .borrow_mut()
                    .entry((namespace.to_string(), name.to_string()))
                    .or_default()
                    .extend(data.clone());
            }
            Ok(())
        }

        async fn delete_config_map(&self, namespace: &str, name: &str) -> Result<Deleted> {
            self.log(format!("delete configmap {namespace}/{name}"));
            Ok(
                if self
                    .config_maps
                    .borrow_mut()
                    .remove(&(namespace.to_string(), name.to_string()))
                    .is_some()
                {
                    Deleted::Removed
                } else {
                    Deleted::Absent
                },
            )
        }
    }
}
