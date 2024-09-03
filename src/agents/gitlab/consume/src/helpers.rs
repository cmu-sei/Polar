/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//TODO: Move to global consumer common lib
pub mod helpers {
    use std::env;
    use common::read_from_env;
    use neo4rs::{Config, ConfigBuilder};
    use url::Url;

    pub fn get_neo4j_endpoint() -> String {
        let endpoint = read_from_env("GRAPH_ENDPOINT".to_owned());
        match Url::parse(endpoint.as_str()) {

           Ok(url) => return url.to_string(),

           Err(e) => panic!("error: {}, the  provided neo4j endpoint is not valid.", e)
        }
    }

    pub fn get_neo_config() -> Config {
        let database_name = read_from_env("GRAPH_DB".to_owned());
        let neo_user = read_from_env("GRAPH_USER".to_owned());
        let neo_password = read_from_env("GRAPH_PASSWORD".to_owned());
        
        let config = ConfigBuilder::new()
        .uri(get_neo4j_endpoint())
        .user(&neo_user)
        .password(&neo_password)
        .db(&database_name)
        .fetch_size(500).max_connections(10).build().unwrap();
        
        return config;
    }

}
