pub mod helpers {
    use std::{env};
    use neo4rs::{Config, ConfigBuilder};
    use url::Url;

    pub fn get_rabbit_endpoint() -> String {
        let endpoint = env::var("RABBITMQ_ENDPOINT").expect("Could not load rabbitmq instance endpoint from enviornment.");
        match Url::parse(endpoint.as_str()) {

           Ok(url) => return url.to_string(),

           Err(e) => panic!("error: {}, the provided rabbitmq endpoint is not valid.", e)
        }
    }

    pub fn get_neo4j_endpoint() -> String {
        let endpoint = env::var("NEO4J_ENDPOINT").expect("Could not load neo4j instance endpoint from enviornment.");
        match Url::parse(endpoint.as_str()) {

           Ok(url) => return url.to_string(),

           Err(e) => panic!("error: {}, the  provided neo4j endpoint is not valid.", e)
        }
    }

    pub fn get_neo_config() -> Config {
        let database_name = env::var("NEO4J_DB").unwrap();
        let neo_user = env::var("NEO4J_USER").unwrap();
        let neo_password = env::var("NEO4J_PASSWORD").unwrap();
        
        let config = ConfigBuilder::new()
        .uri(get_neo4j_endpoint())
        .user(&neo_user)
        .password(&neo_password)
        .db(&database_name)
        .fetch_size(500).max_connections(10).build().unwrap();
        
        return config;
    }

}