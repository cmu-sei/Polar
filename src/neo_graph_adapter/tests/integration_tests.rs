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

// use neo_graph_adapter::{ NeoConfig, NeoQuery, NeoDriver };
// use graph_adapter::{ DriverConfig, DriverType, Connection, GraphQuery, GraphDriver };
// use tokio;

// #[tokio::test]
// async fn test_create_graph_driver() {
//     let driver_config = DriverConfig {
//         driver_config: DriverType::NeoDriver,
//         connection: Connection { uri: "neo4j.labz.s-box.org:7687".to_string(), port: "".to_string(), user: "neo4j".to_string(), pass: "".to_string(), db: "neo4j".to_string() },
//     };
//     let graph_config = NeoConfig {
//         uri: Some(driver_config.connection.uri),
//         user: Some(driver_config.connection.user),
//         password: Some(driver_config.connection.pass),
//         db: Some(driver_config.connection.db),
//         fetch_size: Some(500usize),
//         max_connections: Some(10usize),
//     };

//     let graph_query = NeoQuery::create("RETURN 1;".to_string());

//     let graph_driver = NeoDriver::connect(graph_query, graph_config).await;

//     let the_grapher_driver = neo_graph_adapter::get_type_of(&graph_driver);
//     println!("Config name: {}", the_grapher_driver);

//     assert!(the_grapher_driver == "neo_graph_adapter::NeoDriver");
// }

// #[tokio::test]
// async fn test_execute_graph_driver() {
//     let driver_config = DriverConfig {
//         driver_config: DriverType::NeoDriver,
//         connection: Connection { uri: "localhost:7687".to_string(), port: "".to_string(), user: "neo4j".to_string(), pass: "".to_string(), db: "neo4j".to_string() },
//     };
//     let graph_config = NeoConfig {
//         uri: Some(driver_config.connection.uri),
//         user: Some(driver_config.connection.user),
//         password: Some(driver_config.connection.pass),
//         db: Some(driver_config.connection.db),
//         fetch_size: Some(500usize),
//         max_connections: Some(10usize),
//     };

//     let graph_query = NeoQuery::create("CREATE (n:DavesTestNode { TestNo: \"001\" });".to_string());

//     let graph_driver = &mut NeoDriver::connect(graph_query, graph_config).await;

//     let result = NeoDriver::execute(graph_driver).await;
//     match result {
//         Ok(neoquery) => {
//             let the_query = neo_graph_adapter::get_type_of(&neoquery);
//             println!("Config name: {}", the_query);

//             assert!(the_query == "neo_graph_adapter::NeoQuery");
//         },
//         Err(e) => println!("{}", e)
//     }
// }
