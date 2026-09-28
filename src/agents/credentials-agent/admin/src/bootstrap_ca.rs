//! `bootstrap-ca`: generates the cluster's CA and provisions it into
//! Kubernetes, once.
//!
//! Unlike every other subcommand of this tool it does not publish an
//! event through Cassini: the CA has to exist before cert-issuer can
//! issue anything, and every Cassini connection needs a certificate
//! from it, so there is no broker to publish to yet. It talks to the
//! Kubernetes API directly, with the operator's own kubeconfig -- which
//! is the point. Only someone with cluster-admin rights can create
//! these objects, so the trust root is anchored in the same place as
//! everything else that operator already controls.
//!
//! Creates and never overwrites. A second run fails with 409 rather
//! than replacing the CA, because a new CA silently orphans every
//! certificate already issued. Replacing one is a deliberate act: a
//! cluster admin deletes the Secret first.

use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, PostParams};
use kube::Client;
use std::collections::BTreeMap;

/// Holds the CA certificate and private key. Mounted only into
/// cert-issuer. The infra Dhall must use the same name.
pub const CA_SECRET_NAME: &str = "cert-issuer-ca";

/// Holds the CA certificate only, for every pod that has to verify
/// cert-issuer. Public by nature. The infra Dhall must use the same name.
pub const CA_CONFIGMAP_NAME: &str = "polar-ca";

pub struct BootstrapCaArgs {
    pub common_name: String,
    pub secret_namespace: String,
    pub configmap_namespaces: Vec<String>,
}

/// Builds the objects to create. Pure, so the split between what is
/// secret and what is public can be tested without a cluster: the
/// private key must appear in the Secret and nowhere else.
pub fn build_objects(
    cert_pem: &str,
    key_pem: &str,
    secret_namespace: &str,
    configmap_namespaces: &[String],
) -> (Secret, Vec<ConfigMap>) {
    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(CA_SECRET_NAME.to_string()),
            namespace: Some(secret_namespace.to_string()),
            ..Default::default()
        },
        type_: Some("Opaque".to_string()),
        string_data: Some(BTreeMap::from([
            ("ca.crt".to_string(), cert_pem.to_string()),
            ("ca.key".to_string(), key_pem.to_string()),
        ])),
        ..Default::default()
    };

    let configmaps = configmap_namespaces
        .iter()
        .map(|ns| ConfigMap {
            metadata: ObjectMeta {
                name: Some(CA_CONFIGMAP_NAME.to_string()),
                namespace: Some(ns.clone()),
                ..Default::default()
            },
            data: Some(BTreeMap::from([("ca.pem".to_string(), cert_pem.to_string())])),
            ..Default::default()
        })
        .collect();

    (secret, configmaps)
}

fn is_already_exists(e: &kube::Error) -> bool {
    matches!(e, kube::Error::Api(resp) if resp.code == 409)
}

pub async fn run(args: BootstrapCaArgs) -> Result<(), String> {
    let client = Client::try_default().await.map_err(|e| {
        format!("could not build a Kubernetes client from the current kubeconfig: {e}")
    })?;

    let (cert_pem, key_pem) = cert_issuer_common::ca_root::bootstrap_ca(&args.common_name)?;
    let (secret, configmaps) = build_objects(
        &cert_pem,
        &key_pem,
        &args.secret_namespace,
        &args.configmap_namespaces,
    );

    // The Secret goes first and is the source of truth. If it already
    // exists we stop before creating anything, so a ConfigMap can never
    // end up holding a certificate that doesn't match the key in the
    // Secret.
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &args.secret_namespace);
    match secrets.create(&PostParams::default(), &secret).await {
        Ok(_) => eprintln!(
            "created secret/{CA_SECRET_NAME} in {}",
            args.secret_namespace
        ),
        Err(e) if is_already_exists(&e) => {
            return Err(format!(
                "secret/{CA_SECRET_NAME} already exists in {}; refusing to replace the CA. \
                 Replacing it invalidates every certificate already issued, so a cluster \
                 admin has to delete the Secret first if that is really intended",
                args.secret_namespace
            ));
        }
        Err(e) => return Err(format!("failed to create secret/{CA_SECRET_NAME}: {e}")),
    }

    for cm in &configmaps {
        let ns = cm.metadata.namespace.clone().unwrap_or_default();
        let api: Api<ConfigMap> = Api::namespaced(client.clone(), &ns);
        match api.create(&PostParams::default(), cm).await {
            Ok(_) => eprintln!("created configmap/{CA_CONFIGMAP_NAME} in {ns}"),
            Err(e) => {
                return Err(format!(
                    "secret/{CA_SECRET_NAME} was created, but configmap/{CA_CONFIGMAP_NAME} in \
                     {ns} was not: {e}. The CA now exists and this command will refuse to run \
                     again, so create the missing ConfigMap by hand from the certificate in \
                     the Secret"
                ));
            }
        }
    }

    eprintln!();
    eprintln!("CA provisioned. If secret/{CA_SECRET_NAME} is lost the CA cannot be recovered:");
    eprintln!("a new CA means every certificate in the cluster has to be reissued.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn namespaces() -> Vec<String> {
        vec!["polar".to_string(), "polar-graph".to_string()]
    }

    #[test]
    fn the_secret_holds_both_the_certificate_and_the_key() {
        let (secret, _) = build_objects("CERT", "KEY", "polar", &namespaces());
        let data = secret.string_data.expect("string_data");
        assert_eq!(data.get("ca.crt").map(String::as_str), Some("CERT"));
        assert_eq!(data.get("ca.key").map(String::as_str), Some("KEY"));
        assert_eq!(secret.metadata.name.as_deref(), Some(CA_SECRET_NAME));
        assert_eq!(secret.metadata.namespace.as_deref(), Some("polar"));
    }

    #[test]
    fn the_private_key_appears_in_no_configmap() {
        let (_, configmaps) = build_objects("CERT", "SECRET-KEY-MATERIAL", "polar", &namespaces());
        for cm in configmaps {
            let data = cm.data.expect("data");
            assert!(
                data.values().all(|v| !v.contains("SECRET-KEY-MATERIAL")),
                "a ConfigMap is readable by every pod in its namespace"
            );
            assert_eq!(data.get("ca.pem").map(String::as_str), Some("CERT"));
        }
    }

    #[test]
    fn there_is_one_configmap_per_requested_namespace() {
        let (_, configmaps) = build_objects("CERT", "KEY", "polar", &namespaces());
        let mut got: Vec<_> = configmaps
            .iter()
            .map(|c| c.metadata.namespace.clone().unwrap())
            .collect();
        got.sort();
        assert_eq!(got, vec!["polar".to_string(), "polar-graph".to_string()]);
    }

    #[test]
    fn a_generated_pair_produces_real_pem() {
        let (cert, key) = cert_issuer_common::ca_root::bootstrap_ca("Test CA").unwrap();
        let (secret, _) = build_objects(&cert, &key, "polar", &namespaces());
        let data = secret.string_data.unwrap();
        assert!(data["ca.crt"].contains("BEGIN CERTIFICATE"));
        assert!(data["ca.key"].contains("PRIVATE KEY"));
    }
}
