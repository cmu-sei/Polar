/*
Polar (OSS)

Copyright 2024 Carnegie Mellon University.

NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for full terms.

[DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited distribution.  Please see Copyright notice for non-US Government use and distribution.

This Software includes and/or makes use of Third-Party Software each subject to its own license.

DM24-0470
*/

// #![feature(core_intrinsics)]

#[allow(unused_imports)]
use graph_adapter::{GraphQuery, GraphConfig, GraphDriver, DriverConfig, DriverType, Connection};
use neo4rs::{Graph, Config, Error, Query};
use async_trait::async_trait;

pub struct NeoDriver {
    pub config: NeoConfig,
    pub query: NeoQuery,
    pub graph: Graph,
}

#[derive(Clone)]
pub struct NeoQuery {
    pub the_query: Query,
    pub result: Vec<String>,
}

pub struct NeoConfig {
    pub uri: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub db: Option<String>,
    pub fetch_size: Option<usize>,
    pub max_connections: Option<usize>,
}

impl NeoConfig {
    pub fn uri(mut self, uri: &str) -> Self {
        self.uri = Some(uri.to_owned());
        self
    }
    pub fn user(mut self, user: &str) -> Self {
        self.user = Some(user.to_owned());
        self
    }
    pub fn password(mut self, password: &str) -> Self {
        self.password = Some(password.to_owned());
        self
    }
    pub fn db(mut self, db: &str) -> Self {
        self.db = Some(db.to_owned());
        self
    }
    pub fn fetch_size(mut self, fetch_size: usize) -> Self {
        self.fetch_size = Some(fetch_size);
        self
    }
    pub fn max_connections(mut self, max_connections: usize) -> Self {
        self.max_connections = Some(max_connections);
        self
    }
    pub fn build(self) -> Result<NeoConfig, Error> {
        Ok(NeoConfig {
            uri: self.uri,
            user: self.user,
            password: self.password,
            fetch_size: self.fetch_size,
            max_connections: self.max_connections,
            db: self.db,
        })
    }
}

impl GraphQuery<NeoQuery> for NeoQuery {
    fn create(create_str: String) -> NeoQuery {
        NeoQuery {
            the_query: Query::new(create_str),
            result: Vec::new()
        }
    }
}

impl GraphConfig<Config> for NeoConfig {
    fn create(driver_conf: DriverConfig) -> Config {
        let config = neo4rs::ConfigBuilder::new()
            .uri(driver_conf.connection.uri.as_str())
            .user(driver_conf.connection.user.as_str())
            .password(driver_conf.connection.pass.as_str())
            .db(driver_conf.connection.db.as_str())
            .fetch_size(500usize)
            .max_connections(10usize)
            .build()
            .unwrap();
        return config;
    }
}

#[async_trait]
impl <'a>GraphDriver<'a,NeoDriver,NeoQuery,neo4rs::Error,NeoConfig> for NeoDriver {
    async fn execute(& mut self) -> Result<NeoQuery, neo4rs::Error> {
        let the_query = &self.query.the_query;
        let mut vec: Vec<String> = Vec::new();

        match self.graph.execute(the_query.clone()).await
        {
            Ok(mut _the_result) => {
                match _the_result.next().await {
                    Ok(rslt) => {
                        match rslt {
                            Some(thing) => {
                                let the_value = format!("{:?}",thing);
                                vec.push(the_value);
                            },
                            None => println!("Error")
                        }
                    },
                    _ => {
                        vec.push("Nothing to see here.".to_string());
                    }
                }
                self.query.result = vec;
                return Ok(self.query.clone());
            },
            Err(e) => {
                println!("{:?}", e);
                // return Err(IOError { detail: "Problem with the query...".to_string() });
                return Err(e);
            }
        };
    }

    async fn connect(query: NeoQuery, config: NeoConfig) -> NeoDriver {
        let my_config = neo4rs::ConfigBuilder::new()
            .uri(config.uri.clone().unwrap().as_str())
            .user(config.user.clone().unwrap().as_str())
            .password(config.password.clone().unwrap().as_str())
            .db(config.db.clone().unwrap().as_str())
            .fetch_size(config.fetch_size.unwrap())
            .max_connections(config.max_connections.unwrap())
            .build()
            .unwrap();
        let result = Graph::connect(my_config).await;
        match result {
            Ok(the_graph) => {
                NeoDriver {
                    graph: the_graph,
                    config: config,
                    query: query,
                }
            },
            Err(_) => todo!(), 
        }
    }
}


// pub struct NeoDriver {
    // pub config: NeoConfig,
    // pub query: NeoQuery,
    // pub graph: Graph,
// }
// It's not actually dead code, but the test module gets compiled separately, so the compiler here will complain.
// #[allow(dead_code)]
// pub fn get_type_of<T>(_: &T) -> &str {
//     std::intrinsics::type_name::<T>()
// }

// #[cfg(test)]
// mod tests {
//     use super::*;

//     #[test]
//     fn test_create_neo_query() {
//         let graph_query = super::NeoQuery::create("RETURN 1;".to_string());

//         let the_query = get_type_of(&graph_query);
//         assert!(the_query == "neo_graph_adapter::NeoQuery");
//     }

//     #[test]
//     fn test_create_neo_config() {
//         let driver_config = DriverConfig {
//             driver_config: DriverType::NeoDriver,
//             connection: Connection { uri: "neo4j.cc.cert.org:7687".to_string(), port: "".to_string(), user: "neo4j".to_string(), pass: "example".to_string(), db: "neo4j".to_string() },
//         };
//         let graph_config = super::NeoConfig::create(driver_config);
//         let the_config = get_type_of(&graph_config);
//         println!("Config name: {}", the_config);
//         assert!(the_config == "neo4rs::config::Config");
//     }

// }
