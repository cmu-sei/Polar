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
