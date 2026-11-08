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

//! The real cluster: kube-rs against the named kubeconfig context, and no other.

use crate::cluster::{Applied, Cluster, Deleted};
use crate::plan::{Id, Object};
use crate::secrets::Secret;
use anyhow::{Context, Result, anyhow, bail};
use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::{ConfigMap as KubeConfigMap, Secret as KubeSecret};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, DeleteParams, DynamicObject, Patch, PatchParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::core::GroupVersionKind;
use kube::discovery::{self, ApiCapabilities, ApiResource, Scope};
use kube::{Client, Config, CustomResourceExt};
use polar_operator_common::PolarAgent;
use serde_json::Value as Json;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;

/// The field manager every apply is made under, so that what the tool owns can be seen.
pub const MANAGER: &str = "polar";

pub struct KubeCluster {
    client: Client,
    kinds: Mutex<HashMap<(String, String), (ApiResource, ApiCapabilities)>>,
}

fn split(api_version: &str) -> (&str, &str) {
    api_version.split_once('/').unwrap_or(("", api_version))
}

impl KubeCluster {
    /// Connect to the named context. The kubeconfig's current context is never used: an operator
    /// who means one cluster must say which, so a stale default cannot send a plan to another.
    pub async fn connect(context: &str) -> Result<KubeCluster> {
        let kubeconfig = Kubeconfig::read().context("cannot read the kubeconfig")?;
        if !kubeconfig.contexts.iter().any(|c| c.name == context) {
            let have: Vec<_> = kubeconfig
                .contexts
                .iter()
                .map(|c| c.name.as_str())
                .collect();
            bail!(
                "no kube context named '{context}' (the kubeconfig has: {})",
                have.join(", ")
            );
        }
        let options = KubeConfigOptions {
            context: Some(context.to_string()),
            ..KubeConfigOptions::default()
        };
        let config = Config::from_custom_kubeconfig(kubeconfig, &options)
            .await
            .with_context(|| format!("cannot use the kube context '{context}'"))?;
        let client = Client::try_from(config)?;
        Ok(KubeCluster {
            client,
            kinds: Mutex::new(HashMap::new()),
        })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The API for a kind, or `None` if the cluster does not serve it (yet).
    async fn resolve(
        &self,
        api_version: &str,
        kind: &str,
    ) -> Result<Option<(ApiResource, ApiCapabilities)>> {
        let key = (api_version.to_string(), kind.to_string());
        if let Some(found) = self.kinds.lock().unwrap().get(&key) {
            return Ok(Some(found.clone()));
        }
        let (group, version) = split(api_version);
        match discovery::pinned_kind(&self.client, &GroupVersionKind::gvk(group, version, kind))
            .await
        {
            Ok(found) => {
                self.kinds.lock().unwrap().insert(key, found.clone());
                Ok(Some(found))
            }
            Err(kube::Error::Api(e)) if e.code == 404 => Ok(None),
            Err(kube::Error::Discovery(_)) => Ok(None),
            Err(e) => Err(anyhow!(e).context(format!(
                "cannot look up {kind} ({api_version}) on the cluster"
            ))),
        }
    }

    fn api(
        &self,
        resource: &ApiResource,
        capabilities: &ApiCapabilities,
        namespace: Option<&str>,
    ) -> Result<Api<DynamicObject>> {
        Ok(match capabilities.scope {
            Scope::Cluster => Api::all_with(self.client.clone(), resource),
            Scope::Namespaced => {
                let namespace = namespace.ok_or_else(|| {
                    anyhow!("{} is namespaced but names no namespace", resource.kind)
                })?;
                Api::namespaced_with(self.client.clone(), namespace, resource)
            }
        })
    }
}

fn patch_params(dry_run: bool) -> PatchParams {
    let mut params = PatchParams::apply(MANAGER).force();
    params.dry_run = dry_run;
    params
}

impl Cluster for KubeCluster {
    async fn install_crd(&self, dry_run: bool) -> Result<()> {
        let crd = PolarAgent::crd();
        let name = crd
            .metadata
            .name
            .clone()
            .ok_or_else(|| anyhow!("the CRD has no name"))?;
        let crds: Api<CustomResourceDefinition> = Api::all(self.client.clone());
        crds.patch(&name, &patch_params(dry_run), &Patch::Apply(&crd))
            .await
            .context("cannot install the PolarAgent CRD")?;
        if dry_run {
            return Ok(());
        }
        // Applying it is not serving it: wait until the cluster does.
        for _ in 0..60 {
            if self
                .resolve("polar.sei.cmu.edu/v1alpha1", "PolarAgent")
                .await?
                .is_some()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        bail!("the PolarAgent CRD was applied but the cluster is not serving it after a minute")
    }

    async fn apply(&self, object: &Object, dry_run: bool) -> Result<Applied> {
        let id = object
            .id()
            .ok_or_else(|| anyhow!("an object with no identity"))?;
        let Some((resource, capabilities)) = self.resolve(&id.api_version, &id.kind).await? else {
            if dry_run {
                return Ok(Applied::UnknownKind);
            }
            bail!(
                "the cluster does not know the kind {} ({}); is its CRD installed by an earlier stage?",
                id.kind,
                id.api_version
            );
        };
        let api = self.api(&resource, &capabilities, id.namespace.as_deref())?;
        api.patch(&id.name, &patch_params(dry_run), &Patch::Apply(&object.0))
            .await
            .with_context(|| format!("cannot apply {id}"))?;
        Ok(Applied::Done)
    }

    async fn delete(&self, id: &Id) -> Result<Deleted> {
        let Some((resource, capabilities)) = self.resolve(&id.api_version, &id.kind).await? else {
            return Ok(Deleted::Absent);
        };
        let api = self.api(&resource, &capabilities, id.namespace.as_deref())?;
        match api.delete(&id.name, &DeleteParams::default()).await {
            Ok(_) => Ok(Deleted::Removed),
            Err(kube::Error::Api(e)) if e.code == 404 => Ok(Deleted::Absent),
            Err(e) => Err(anyhow!(e).context(format!("cannot delete {id}"))),
        }
    }

    async fn get(&self, id: &Id) -> Result<Option<Json>> {
        let Some((resource, capabilities)) = self.resolve(&id.api_version, &id.kind).await? else {
            return Ok(None);
        };
        let api = self.api(&resource, &capabilities, id.namespace.as_deref())?;
        match api.get_opt(&id.name).await {
            Ok(found) => Ok(found.map(serde_json::to_value).transpose()?),
            Err(e) => Err(anyhow!(e).context(format!("cannot read {id}"))),
        }
    }

    async fn secret(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>> {
        let secrets: Api<KubeSecret> = Api::namespaced(self.client.clone(), namespace);
        match secrets.get_opt(name).await {
            Ok(None) => Ok(None),
            Ok(Some(found)) => Ok(Some(
                found
                    .data
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|(key, ByteString(bytes))| {
                        String::from_utf8(bytes).ok().map(|text| (key, text))
                    })
                    .collect(),
            )),
            // A namespace that is not there yet holds no secrets.
            Err(kube::Error::Api(e)) if e.code == 404 => Ok(None),
            Err(e) => Err(anyhow!(e).context(format!("cannot read secret {namespace}/{name}"))),
        }
    }

    async fn create_secret(
        &self,
        namespace: &str,
        name: &str,
        data: &BTreeMap<String, Secret>,
        dry_run: bool,
    ) -> Result<()> {
        let secret = KubeSecret {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                labels: Some(BTreeMap::from([(
                    "app.kubernetes.io/managed-by".to_string(),
                    MANAGER.to_string(),
                )])),
                ..ObjectMeta::default()
            },
            string_data: Some(
                data.iter()
                    .map(|(k, v)| (k.clone(), v.expose().to_string()))
                    .collect(),
            ),
            type_: Some("Opaque".to_string()),
            ..KubeSecret::default()
        };
        let secrets: Api<KubeSecret> = Api::namespaced(self.client.clone(), namespace);
        let params = PostParams {
            dry_run,
            ..PostParams::default()
        };
        secrets
            .create(&params, &secret)
            .await
            .with_context(|| format!("cannot create secret {namespace}/{name}"))?;
        Ok(())
    }

    async fn delete_secret(&self, namespace: &str, name: &str) -> Result<Deleted> {
        let secrets: Api<KubeSecret> = Api::namespaced(self.client.clone(), namespace);
        match secrets.delete(name, &DeleteParams::default()).await {
            Ok(_) => Ok(Deleted::Removed),
            Err(kube::Error::Api(e)) if e.code == 404 => Ok(Deleted::Absent),
            Err(e) => Err(anyhow!(e).context(format!("cannot delete secret {namespace}/{name}"))),
        }
    }

    async fn config_map(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>> {
        let config_maps: Api<KubeConfigMap> = Api::namespaced(self.client.clone(), namespace);
        match config_maps.get_opt(name).await {
            Ok(found) => Ok(found.map(|c| c.data.unwrap_or_default())),
            // A namespace that is not there yet holds no ConfigMaps.
            Err(kube::Error::Api(e)) if e.code == 404 => Ok(None),
            Err(e) => Err(anyhow!(e).context(format!("cannot read configmap {namespace}/{name}"))),
        }
    }

    async fn write_config_map(
        &self,
        namespace: &str,
        name: &str,
        data: &BTreeMap<String, String>,
        dry_run: bool,
    ) -> Result<()> {
        let config_map = KubeConfigMap {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                labels: Some(BTreeMap::from([(
                    "app.kubernetes.io/managed-by".to_string(),
                    MANAGER.to_string(),
                )])),
                ..ObjectMeta::default()
            },
            data: Some(data.clone()),
            ..KubeConfigMap::default()
        };
        let config_maps: Api<KubeConfigMap> = Api::namespaced(self.client.clone(), namespace);
        config_maps
            .patch(name, &patch_params(dry_run), &Patch::Apply(&config_map))
            .await
            .with_context(|| format!("cannot write configmap {namespace}/{name}"))?;
        Ok(())
    }

    async fn delete_config_map(&self, namespace: &str, name: &str) -> Result<Deleted> {
        let config_maps: Api<KubeConfigMap> = Api::namespaced(self.client.clone(), namespace);
        match config_maps.delete(name, &DeleteParams::default()).await {
            Ok(_) => Ok(Deleted::Removed),
            Err(kube::Error::Api(e)) if e.code == 404 => Ok(Deleted::Absent),
            Err(e) => {
                Err(anyhow!(e).context(format!("cannot delete configmap {namespace}/{name}")))
            }
        }
    }
}
