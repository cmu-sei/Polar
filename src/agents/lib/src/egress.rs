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

//! The way out of the cluster: the proxy an agent's calls outside the cluster go through, the hosts it reaches
//! directly instead, and the CA certificates it trusts beside the system's (docs/architecture/egress.md).
//!
//! An agent that calls outside the cluster declares this section, and the render writes the target's
//! `cluster.egress` into it. A target that sets nothing leaves every variable unset: the agent then calls out
//! directly, trusting the system's roots, as it did before the section existed. The process's own `HTTPS_PROXY`,
//! `HTTP_PROXY` and `NO_PROXY` are never consulted, so the document is the one place a proxy is named.

use polar_env::{Config, ConfigError, Described, Env, Optional, Section};
use reqwest::{Certificate, ClientBuilder, NoProxy, Proxy};
use url::Url;

pub static PROXY: Optional<String> = Optional::text(
    "POLAR_EGRESS_PROXY",
    "the proxy every call outside the cluster goes through; unset, calls go direct and the process's \
     HTTP(S)_PROXY is not consulted",
);

pub static NO_PROXY: Optional<String> = Optional::text(
    "POLAR_EGRESS_NO_PROXY",
    "what is reached directly, not through POLAR_EGRESS_PROXY: comma-separated hosts, .domain suffixes, \
     IP addresses and CIDRs",
);

pub static CA_CERT: Optional<String> = Optional::text(
    "POLAR_EGRESS_CA_CERT",
    "PEM CA certificates trusted beside the system's; set and unreadable or empty stops the agent",
);

static VARIABLES: &[&dyn Described] = &[&PROXY, &NO_PROXY, &CA_CERT];

/// The section an agent that calls outside the cluster registers. The render writes the target's egress setting
/// only into an agent that declares it.
pub static SECTION: Section = Section {
    name: "egress",
    vars: VARIABLES,
    relations,
};

fn relations(env: &dyn Env) -> Vec<String> {
    let proxy = PROXY.read(env).ok().flatten();
    let no_proxy = NO_PROXY.read(env).ok().flatten();
    match (proxy, no_proxy) {
        (Some(proxy), _) => proxy_url(&proxy).err().into_iter().collect(),
        (None, Some(_)) => vec![format!(
            "{} is set but {} is not: there is nothing to bypass",
            NO_PROXY.name(),
            PROXY.name()
        )],
        (None, None) => Vec::new(),
    }
}

/// The proxy as a URL: http or https, with a host, and no user or password (the setting is written into a
/// ConfigMap, so a proxy that needs credentials is not supported). A port left out is the scheme's.
pub fn proxy_url(raw: &str) -> Result<Url, String> {
    let credentials = || {
        format!(
            "{} names a user or password: a proxy that needs credentials is not supported",
            PROXY.name()
        )
    };
    let shape = || {
        format!(
            "{} is '{raw}': a proxy is an http:// or https:// URL with a host and a port, such as \
             http://proxy.example.org:3128",
            PROXY.name()
        )
    };
    let url = match Url::parse(raw.trim()) {
        Ok(url) => url,
        // What does not parse is not repeated if it may hold a password.
        Err(_) if raw.contains('@') => return Err(credentials()),
        Err(_) => return Err(shape()),
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Err(credentials());
    }
    let has_host = url.host_str().is_some_and(|h| !h.is_empty());
    if !matches!(url.scheme(), "http" | "https")
        || !has_host
        || url.port_or_known_default().is_none()
    {
        return Err(shape());
    }
    Ok(url)
}

/// What stops an agent whose egress CA cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error("POLAR_EGRESS_CA_CERT {path} cannot be read: {source}")]
    Unreadable {
        path: String,
        source: std::io::Error,
    },
    #[error("POLAR_EGRESS_CA_CERT {path} holds no PEM certificate")]
    Empty { path: String },
    #[error("POLAR_EGRESS_CA_CERT {path} holds a certificate that cannot be used: {reason}")]
    Invalid { path: String, reason: String },
}

/// The egress settings an agent was given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Egress {
    pub proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub ca_cert: Option<String>,
}

impl Egress {
    pub fn read(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            proxy: config.read(&PROXY)?,
            no_proxy: config.read(&NO_PROXY)?,
            ca_cert: config.read(&CA_CERT)?,
        })
    }

    /// The CA file's path and content, or none when no CA is set.
    async fn ca_file(&self) -> Result<Option<(String, Vec<u8>)>, EgressError> {
        let Some(path) = &self.ca_cert else {
            return Ok(None);
        };
        let data = tokio::fs::read(path)
            .await
            .map_err(|source| EgressError::Unreadable {
                path: path.clone(),
                source,
            })?;
        Ok(Some((path.clone(), data)))
    }

    /// Every certificate in the CA file, to trust beside the system's roots: none when no CA is set. A file that
    /// cannot be read, holds no certificate or holds one that does not parse is an error, so a trust root that
    /// was asked for is never left out in silence.
    pub async fn ca_bundle(&self) -> Result<Vec<Certificate>, EgressError> {
        let Some((path, data)) = self.ca_file().await? else {
            return Ok(Vec::new());
        };
        let certs = Certificate::from_pem_bundle(&data).map_err(|e| EgressError::Invalid {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        if certs.is_empty() {
            return Err(EgressError::Empty { path });
        }
        Ok(certs)
    }

    /// Each certificate block of the CA file as PEM text of its own, for a client that reads only the first
    /// certificate of what it is given (oci-client's `Certificate`): none when no CA is set.
    pub async fn pem_blocks(&self) -> Result<Vec<Vec<u8>>, EgressError> {
        let Some((path, data)) = self.ca_file().await? else {
            return Ok(Vec::new());
        };
        let blocks = pem_blocks_of(&String::from_utf8_lossy(&data));
        if blocks.is_empty() {
            return Err(EgressError::Empty { path });
        }
        Ok(blocks)
    }

    /// A reqwest client builder that trusts `bundle` beside the system's roots and goes out through the proxy,
    /// or directly when none is set. The TLS backend is left as it is: reqwest's default, native-tls, reads the
    /// system's roots from the image (`SSL_CERT_FILE`), and rustls would trust its built-in list in their place.
    /// Without a proxy the system proxy is switched off, so the process environment never decides the way out.
    pub fn configure(
        &self,
        mut builder: ClientBuilder,
        bundle: &[Certificate],
    ) -> Result<ClientBuilder, reqwest::Error> {
        for cert in bundle {
            builder = builder.add_root_certificate(cert.clone());
        }
        Ok(match &self.proxy {
            Some(proxy) => builder.proxy(
                Proxy::all(proxy)?
                    .no_proxy(NoProxy::from_string(self.no_proxy.as_deref().unwrap_or(""))),
            ),
            None => builder.no_proxy(),
        })
    }

    /// The proxy to reach `url` through, by the same rules the reqwest client follows: none when no proxy is set,
    /// when the host is in POLAR_EGRESS_NO_PROXY, or when the URL is not http or https (an ssh remote, or a URL
    /// that does not parse). For a client that is told its proxy per connection, such as git.
    pub fn proxy_for(&self, url: &str) -> Option<String> {
        let proxy = self.proxy.as_ref()?;
        let uri: http::Uri = url.parse().ok()?;
        hyper_util::client::proxy::matcher::Matcher::builder()
            .http(proxy)
            .https(proxy)
            .no(self.no_proxy.clone().unwrap_or_default())
            .build()
            .intercept(&uri)
            .map(|_| proxy.clone())
    }
}

const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const END: &str = "-----END CERTIFICATE-----";

/// Each `CERTIFICATE` block of `text`, from its BEGIN line to its END line.
fn pem_blocks_of(text: &str) -> Vec<Vec<u8>> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        let Some(len) = rest[start..].find(END) else {
            break;
        };
        let end = start + len + END.len();
        blocks.push(format!("{}\n", &rest[start..end]).into_bytes());
        rest = &rest[end..];
    }
    blocks
}

/// Test certificates, TLS servers and child processes for the tests of egress, in this crate and in the agents
/// that use it.
#[cfg(feature = "testing")]
pub mod testing {
    use std::path::{Path, PathBuf};
    use std::process::Output;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::process::Command;

    /// A CA made for a test.
    pub struct TestCa {
        pub pem: String,
        issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
    }

    impl TestCa {
        pub fn new(name: &str) -> Self {
            let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, name);
            let key = rcgen::KeyPair::generate().unwrap();
            let pem = params.self_signed(&key).unwrap().pem();
            Self {
                pem,
                issuer: rcgen::Issuer::new(params, key),
            }
        }

        /// Writes the CA certificate to `dir/file` and returns the path.
        pub fn write(&self, dir: &Path, file: &str) -> PathBuf {
            let path = dir.join(file);
            std::fs::write(&path, &self.pem).unwrap();
            path
        }
    }

    /// An https server of a test.
    pub struct TestServer {
        /// Its address, `https://127.0.0.1:<port>`.
        pub url: String,
        served: Arc<AtomicUsize>,
    }

    impl TestServer {
        /// How many requests it has been sent over TLS.
        pub fn served(&self) -> usize {
            self.served.load(Ordering::SeqCst)
        }
    }

    /// An https server on 127.0.0.1 with a certificate for 127.0.0.1 and localhost that `ca` signed. It answers
    /// every request with 200 and `ok` until the runtime ends.
    pub async fn https_server(ca: &TestCa) -> TestServer {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params =
            rcgen::CertificateParams::new(vec!["127.0.0.1".to_string(), "localhost".to_string()])
                .unwrap();
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let cert = params.signed_by(&key, &ca.issuer).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = Arc::new(AtomicUsize::new(0));
        let count = served.clone();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                let count = count.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut buf = [0u8; 4096];
                    if tls.read(&mut buf).await.unwrap_or(0) > 0 {
                        count.fetch_add(1, Ordering::SeqCst);
                    }
                    let _ = tls
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                        )
                        .await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        TestServer {
            url: format!("https://127.0.0.1:{port}"),
            served,
        }
    }

    /// A proxy that refuses everything and keeps the first line of each request it is sent (`CONNECT host:443
    /// HTTP/1.1` for an https destination). Returns its address and the lines.
    pub async fn recording_proxy() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut tcp, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let n = tcp.read(&mut buf).await.unwrap_or(0);
                let text = String::from_utf8_lossy(&buf[..n]);
                record
                    .lock()
                    .unwrap()
                    .push(text.lines().next().unwrap_or("").to_string());
                let _ = tcp
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    /// Runs the test `name` of the running test binary again, alone, in a child process with `env` set as well
    /// as what this process has: for what reads the process environment, which a test must not change in place.
    pub async fn rerun(name: &str, env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([name, "--exact", "--nocapture", "--test-threads=1"]);
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().await.unwrap()
    }

    /// A variable of the process environment, read where a test that `rerun` started finds what it was given.
    pub fn given(name: &str) -> Option<String> {
        polar_env::Env::var(&polar_env::ProcessEnv, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> Config {
        let env: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::builder(&env).register(&SECTION).build()
    }

    fn problems(pairs: &[(&str, &str)]) -> Vec<String> {
        config(pairs).check()
    }

    #[test]
    fn the_section_is_sound_and_its_defaults_parse() {
        assert_eq!(SECTION.defaults_problems(), Vec::<String>::new());
    }

    #[test]
    fn unset_is_no_egress_and_no_problem() {
        let egress = Egress::read(&config(&[])).unwrap();
        assert_eq!(egress, Egress::default());
        assert_eq!(egress.proxy_for("https://github.com/x"), None);
        assert!(problems(&[]).is_empty());
    }

    #[test]
    fn the_section_declares_the_proxy_the_bypass_list_and_the_ca() {
        let names: Vec<&str> = SECTION.describe().iter().map(|v| v.name).collect();
        assert_eq!(
            names,
            [
                "POLAR_EGRESS_PROXY",
                "POLAR_EGRESS_NO_PROXY",
                "POLAR_EGRESS_CA_CERT"
            ]
        );
    }

    #[test]
    fn an_http_or_https_proxy_with_a_host_and_a_port_is_accepted() {
        for proxy in [
            "http://proxy.corp.example:3128",
            "https://proxy.corp.example:8443",
            "http://10.20.0.5:3128/",
            "http://[fd00::1]:3128",
        ] {
            assert!(
                problems(&[("POLAR_EGRESS_PROXY", proxy)]).is_empty(),
                "{proxy}"
            );
            let egress = Egress::read(&config(&[("POLAR_EGRESS_PROXY", proxy)])).unwrap();
            assert_eq!(egress.proxy.as_deref(), Some(proxy));
        }
    }

    #[test]
    fn a_socks_proxy_a_proxy_with_credentials_and_one_without_a_host_are_refused() {
        for (proxy, says) in [
            ("socks5://proxy.corp.example:1080", "is 'socks5://"),
            ("ftp://proxy.corp.example:21", "is 'ftp://"),
            ("proxy.corp.example:3128", "an http:// or https:// URL"),
            ("http://", "an http:// or https:// URL"),
            ("http://:3128", "an http:// or https:// URL"),
            ("not a url", "an http:// or https:// URL"),
            (
                "http://user:pw@proxy.corp.example:3128",
                "names a user or password",
            ),
            (
                "http://user@proxy.corp.example:3128",
                "names a user or password",
            ),
        ] {
            let found = problems(&[("POLAR_EGRESS_PROXY", proxy)]);
            assert_eq!(found.len(), 1, "{proxy}: {found:?}");
            assert!(found[0].contains(says), "{proxy}: {found:?}");
            assert!(
                !found[0].contains("pw"),
                "a password is never repeated: {found:?}"
            );
        }
    }

    #[test]
    fn a_bypass_list_without_a_proxy_is_a_problem() {
        let found = problems(&[("POLAR_EGRESS_NO_PROXY", ".corp.example")]);
        assert_eq!(
            found,
            [
                "POLAR_EGRESS_NO_PROXY is set but POLAR_EGRESS_PROXY is not: there is nothing to bypass"
            ]
        );
        assert!(
            problems(&[
                ("POLAR_EGRESS_PROXY", "http://proxy:3128"),
                ("POLAR_EGRESS_NO_PROXY", ".corp.example")
            ])
            .is_empty()
        );
    }

    #[test]
    fn a_ca_only_needs_no_proxy() {
        assert!(problems(&[("POLAR_EGRESS_CA_CERT", "/etc/polar-egress-ca/ca.pem")]).is_empty());
    }

    #[test]
    fn the_blocks_of_a_bundle_are_split_one_per_certificate() {
        let text = format!("junk\n{BEGIN}\nAAAA\n{END}\nmore\n{BEGIN}\nBBBB\n{END}\n");
        let blocks = pem_blocks_of(&text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], format!("{BEGIN}\nAAAA\n{END}\n").into_bytes());
        assert_eq!(blocks[1], format!("{BEGIN}\nBBBB\n{END}\n").into_bytes());
        assert!(pem_blocks_of("no certificate here").is_empty());
        assert!(pem_blocks_of(&format!("{BEGIN}\nunterminated")).is_empty());
    }

    #[tokio::test]
    async fn a_ca_that_is_set_and_cannot_be_read_or_holds_nothing_names_the_variable_and_the_path()
    {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.pem").display().to_string();
        let egress = Egress {
            ca_cert: Some(missing.clone()),
            ..Egress::default()
        };
        let error = egress.ca_bundle().await.unwrap_err().to_string();
        assert!(
            error.contains("POLAR_EGRESS_CA_CERT") && error.contains(&missing),
            "{error}"
        );

        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, "").unwrap();
        let egress = Egress {
            ca_cert: Some(empty.display().to_string()),
            ..Egress::default()
        };
        let error = egress.ca_bundle().await.unwrap_err().to_string();
        assert!(
            error.contains("POLAR_EGRESS_CA_CERT") && error.contains("holds no PEM certificate"),
            "{error}"
        );
        assert!(egress.pem_blocks().await.is_err());
    }

    #[tokio::test]
    async fn no_ca_is_an_empty_bundle() {
        let egress = Egress::default();
        assert!(egress.ca_bundle().await.unwrap().is_empty());
        assert!(egress.pem_blocks().await.unwrap().is_empty());
    }
}
