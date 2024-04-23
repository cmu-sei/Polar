// Polar
// Copyright 2023 Carnegie Mellon University.
// NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.
// [DISTRIBUTION STATEMENT D] Distribution authorized to the Department of Defense and U.S. DoD contractors only (materials contain software documentation) (determination date: 2022-05-20). Other requests shall be referred to Defense Threat Reduction Agency.
// Notice to DoD Subcontractors:  This document may contain Covered Defense Information (CDI).  Handling of this information is subject to the controls identified in DFARS 252.204-7012 – SAFEGUARDING COVERED DEFENSE INFORMATION AND CYBER INCIDENT REPORTING
// Carnegie Mellon® is registered in the U.S. Patent and Trademark Office by Carnegie Mellon University.
// This Software includes and/or makes use of Third-Party Software subject to its own license, see license.txt file for more information. 
// DM23-0821
// 
pub mod helpers {
    use std::env;
    use neo4rs::{Config, ConfigBuilder};
    use url::Url;

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
