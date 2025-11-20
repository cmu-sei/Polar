use cassini_client::TCPClientConfig;
use harness_common::{Agent, TestPlan};
use ractor::concurrency::Duration;
use reqwest::StatusCode;
use serde::Serialize;
use std::{collections::HashMap, path::Display};
use testcontainers::{
    core::{wait::HttpWaitStrategy, WaitFor},
    Image,
};

// default path for tls certs to be stashed
pub const TLS_PATH: &str = "/etc/tls";
pub const CA_CERT: &str = "ca_certificate.pem";
pub const SERVER_CERTIFICATE: &str = "server_chain.pem";
pub const SERVER_KEY: &str = "server.key";
pub const CLIENT_CERTIFICATE: &str = "client_cert.pem";
pub const CLIENT_KEY: &str = "client.key";

#[derive(Debug, Default)]
pub struct CassiniImage {
    pub tag: String,
    pub env: HashMap<String, String>,
    pub tcp_port: u16,
    pub http_port: u16,
}

impl CassiniImage {
    pub fn new(tag: impl Into<String>) -> Self {
        CassiniImage {
            tag: tag.into(),
            env: HashMap::new(),
            // easy defaults, we may want to change eventually
            // see cassini's src for details
            tcp_port: 8080,
            http_port: 3000,
        }
    }
    pub fn with_env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.env.insert(key.into(), val.into());
        self
    }

    pub fn with_tcp(mut self, port: u16) -> Self {
        self.tcp_port = port;
        self
    }
    pub fn with_http(mut self, port: u16) -> Self {
        self.http_port = port;
        self
    }
}

impl Image for CassiniImage {
    fn name(&self) -> &str {
        "cassini"
    }

    fn tag(&self) -> &str {
        &self.tag
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        // TODO: This keeps entering a blocking loop, constantly pinging the service instead of continuing. Why?
        // for now, ping healthcheck api and check we got an ok message
        // let http = HttpWaitStrategy::new("/api/health")
        //     .with_port(self.http_port.into())
        //     .with_expected_status_code(StatusCode::OK);
        // vec![WaitFor::Http(Box::new(http))]
        vec![]
    }
}

#[derive(Debug, Default)]
pub struct AgentImage {
    pub name: String,
    pub tag: String,
    pub env: Vec<(String, String)>,
    pub mounts: Vec<(String, String)>,
    pub wait_for: Vec<WaitFor>,
    pub expose_ports: Option<Vec<u16>>,
}

impl AgentImage {
    pub fn from_agent(agent: &Agent) -> Self {
        // set up common env vars and mounts
        // all agents are going to be cassini clients, so get that out the way

        // set paths for where to mount certs into the images.
        let ca_cert_path = format!("{}/{}", TLS_PATH, CA_CERT);
        let client_cert_path = format!("{}/{}", TLS_PATH, CLIENT_CERTIFICATE);
        let client_key_path = format!("{}/{}", TLS_PATH, CLIENT_KEY);

        let mut env = vec![
            ("TLS_CLIENT_KEY".to_string(), client_key_path.clone()),
            ("TLS_CA_CERT".to_string(), ca_cert_path.clone()),
            ("TLS_CLIENT_CERT".to_string(), client_cert_path.clone()),
        ];

        // ----- CAUTION -----
        // kinda hacky, use an existing tls config impl to read TLS cert paths from the env we're already running in.
        // At the moment, the harness clients are doing the same thing all other cassini clients do, reading TLS cert paths from the env.
        // It might make more sense to read them from the test plan, but until that becomes needed...
        // ----- CAUTION -----
        let tls = TCPClientConfig::new();

        // mounts
        let mounts = vec![
            (tls.ca_certificate_path, ca_cert_path),
            (tls.client_certificate_path, client_cert_path),
            (tls.client_key_path, client_key_path),
        ];

        match agent {
            Agent::ProducerAgent { .. } => {
                let mut producer_env = vec![("RUST_LOG".into(), "info".into())];
                // append new vars
                env.append(&mut producer_env);

                AgentImage {
                    name: "harness-producer".into(),
                    tag: "latest".into(),
                    env,
                    mounts,
                    wait_for: vec![],
                    expose_ports: None,
                }
            }
            Agent::SinkAgent => {
                let mut sink_env = vec![("RUST_LOG".into(), "info".into())];
                // append new vars
                env.append(&mut sink_env);

                AgentImage {
                    name: "harness-sink".into(),
                    tag: "latest".into(),
                    env,
                    mounts,
                    wait_for: vec![],
                    expose_ports: None,
                }
            }
            Agent::LinkerAgent => {
                let mut linker_env = vec![("RUST_LOG".into(), "info".into())];
                // append new vars
                env.append(&mut linker_env);

                AgentImage {
                    name: "provenance-linker".into(),
                    tag: "latest".into(),
                    env,
                    mounts,
                    wait_for: vec![],
                    expose_ports: None,
                }
            }
            Agent::ResolverAgent => {
                let mut resolver_env = vec![("RUST_LOG".into(), "info".into())];
                // append new vars
                env.append(&mut resolver_env);

                AgentImage {
                    name: "provenance-resolver".into(),
                    tag: "latest".into(),
                    env,
                    mounts,
                    wait_for: vec![],
                    expose_ports: None,
                }
            }
            Agent::GitlabConsumer => AgentImage {
                name: "gitlab-consumer".into(),
                tag: "latest".into(),
                env: vec![],
                mounts: vec![],
                wait_for: vec![],
                expose_ports: None,
            },
            _ => todo!("Implement handklers for other agent types"),
        }
    }
}

impl Image for AgentImage {
    fn name(&self) -> &str {
        &self.name
    }

    fn tag(&self) -> &str {
        &self.tag
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        self.wait_for.clone()
    }
}
