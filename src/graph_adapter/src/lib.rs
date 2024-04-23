use async_trait::async_trait;
use serde::Deserialize;

#[derive(Deserialize,Clone)]
pub enum DriverType {
    NeoDriver,
}

// It sort of depends on the database connector, but some connectors may have
// the port as part of the URI, some separate it. I'm leaving us options that
// avoid parsing later. We might want to create fields for protocols, too.
#[derive(Deserialize,Clone)]
pub struct Connection {
    pub uri: String,
    pub port: String,
    pub user: String,
    pub pass: String,
    pub db: String,
}

#[derive(Deserialize,Clone)]
pub struct DriverConfig {
    pub driver_config: DriverType,
    pub connection: Connection,
}

pub trait GraphQuery<T> {
    fn create(create_str: String) -> T;
}

pub trait GraphConfig<S> {
    fn create(driver_conf: DriverConfig) -> S;
}
#[async_trait]
pub trait GraphDriver<'a,S,T,E,W> {
    async fn execute(& mut self) -> 
        Result<T, E>;

    async fn connect(query: T, config: W) -> S;
}