/*
Polar (OSS)

Copyright 2024 Carnegie Mellon University.

NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for full terms.

[DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited distribution.  Please see Copyright notice for non-US Government use and distribution.

This Software includes and/or makes use of Third-Party Software each subject to its own license.

DM24-0470
*/

use std::fs;
use serde::Deserialize;
use neo4rs::*;

// TOML Data Struct
#[derive(Deserialize,Clone)]
struct MyToml {
    connection: Connection,
    task: Task,
}

// TOML Data Struct
#[derive(Deserialize,Clone)]
struct Connection {
    uri: String,
    user: String,
    pass: String,
}

// TOML Data Struct
#[derive(Deserialize,Clone)]
struct Task {
    csv_path: String,
}

// CSV Data Struct
// We don't control the header case...
#[allow(non_snake_case)]
#[derive(Debug, serde::Deserialize)]
struct Record {
    Name: String,
    Flow: String,
}

#[tokio::main]
async fn main() {
    // The name of the config file is hard-coded. I think this is fine for our needs.
    let config_file = fs::read_to_string("./neo.toml");

    match config_file {
        // Config file is now a string.
        Ok(config_as_string) => {

            // Config file string is now a TOML document.
            let toml: MyToml = toml::from_str(&config_as_string).unwrap();

            let config = config()
            .uri(toml.connection.uri.as_str())
            .user(toml.connection.user.as_str())
            .password(toml.connection.pass.as_str())
            .db("neo4j")
            .fetch_size(500)
            .max_connections(10)
            .build()
            .unwrap();
            // Creating the graph in a separate function is not happening.
            let graph = Graph::connect(config).await.unwrap();

            let mut iter = get_csv_reader_iter(toml.task.csv_path);
            for result in iter.deserialize() {
                let record: Record = result.unwrap();
                let split_flow = record.Flow.split("|");
                let mut previous_activity = "";
                for (i, n) in split_flow.enumerate() {
                    // We want chains that look like this:
                    // (:Process)-[:Step]->(:Activity)-[:Step]->(:Activity)-[:Step]->(:Activity)
                    // First iteration, create the Process node, based on the
                    // name. Also during the first iteration, the chaining logic
                    // is a little different. We don't need the previous
                    // Activity, rather we need only the Process. Consecutive
                    // iterations, we need to grab the name for the previous
                    // Activity or ActivityCategory, from the split_flow, using
                    // the index-1. Chain until there are no more elements to
                    // chain.

                    // TODO: So, I hate this code. Lots of copy pasta with
                    // minimal changes. Unfortunately, writing this in a way
                    // that doesn't have lots of repetition would require a
                    // priori knowledge and selection of which query was going
                    // to run successfully. I.e., until we actually query the
                    // DB, we don't know which query is going to be accepted.
                    // Some of these are simply not going to match a valid
                    // combination of nodes. I attempted to yield on a match but
                    // that seems to cause unanticipated problems. Recommend
                    // leaving this alone until someone figures out how to know
                    // what types are associated with the input data, without
                    // that information being available...
                    match i {
                        // First time, create Process nodes, as well as the first step.
                        0 => {
                            let the_query = query(format!(
                                "MATCH (a:ActivityCategory {{Name: \"{}\"}}), (b:ActivityCategory {{Name: \"{}\"}}) 
                                 CREATE (l:Process {{Name: \"{}\"}})-[:RefersTo]->(a) 
                                 WITH b,l 
                                 CREATE p=(l)-[:Step {{Sequence: \"{}\", InProcess: \"{}\"}}]->(b) 
                                 RETURN p", record.Name, n, record.Name, i+1, record.Name).as_str());
                            execute_query(&graph, the_query).await;

                            let the_query = query(format!(
                                "MATCH (a:Activity {{Name: \"{}\"}}), (b:Activity {{Name: \"{}\"}}) 
                                 CREATE (l:Process {{Name: \"{}\"}})-[:RefersTo]->(a) 
                                 WITH b,l 
                                 CREATE p=(l)-[:Step {{Sequence: \"{}\", InProcess: \"{}\"}}]->(b) 
                                 RETURN p", record.Name, n, record.Name, i+1, record.Name).as_str());
                            execute_query(&graph, the_query).await;

                            let the_query = query(format!(
                                "MATCH (a:ActivityCategory {{Name: \"{}\"}}), (b:Activity {{Name: \"{}\"}}) 
                                 CREATE (l:Process {{Name: \"{}\"}})-[:RefersTo]->(a) 
                                 WITH b,l 
                                 CREATE p=(l)-[:Step {{Sequence: \"{}\", InProcess: \"{}\"}}]->(b) 
                                 RETURN p", record.Name, n, record.Name, i+1, record.Name).as_str());
                            execute_query(&graph, the_query).await;
                        },
                        _ => {
                            let the_query = query(format!(
                                "MATCH (a:Activity {{Name: \"{}\"}}), (b:Activity {{Name: \"{}\"}}) 
                                 CREATE p=(a)-[:Step {{Sequence: \"{}\", InProcess: \"{}\"}}]->(b) 
                                 RETURN p", previous_activity, n, i+1, record.Name).as_str());
                            execute_query(&graph, the_query).await;
                            
                            let the_query = query(format!(
                                "MATCH (a:ActivityCategory {{Name: \"{}\"}}), (b:Activity {{Name: \"{}\"}}) 
                                 CREATE p=(a)-[:Step {{Sequence: \"{}\", InProcess: \"{}\"}}]->(b) 
                                 RETURN p", previous_activity, n, i+1, record.Name).as_str());
                            execute_query(&graph, the_query).await;

                            let the_query = query(format!(
                                "MATCH (a:Activity {{Name: \"{}\"}}), (b:ActivityCategory {{Name: \"{}\"}}) 
                                 CREATE p=(a)-[:Step {{Sequence: \"{}\", InProcess: \"{}\"}}]->(b) 
                                 RETURN p", previous_activity, n, i+1, record.Name).as_str());
                            execute_query(&graph, the_query).await;

                            let the_query = query(format!(
                                "MATCH (a:ActivityCategory {{Name: \"{}\"}}), (b:ActivityCategory {{Name: \"{}\"}}) 
                                 CREATE p=(a)-[:Step {{Sequence: \"{}\", InProcess: \"{}\"}}]->(b) 
                                 RETURN p", previous_activity, n, i+1, record.Name).as_str());
                            execute_query(&graph, the_query).await;
                        }
                    }
                    previous_activity = n.clone();
                }
            }
        }
        Err(error) => {
            println!("{}", error);
        }

    }
}

async fn execute_query(graph: &Graph, the_query: Query) -> Option<Row> {
    match graph.execute(the_query).await
    {
        Ok(mut _the_result) => {
            match _the_result.next().await {
                Ok(rslt) => {
                    // println!("Result: {:?}",rslt);
                    return rslt;
                },
                _ => return None
            }
        },
        Err(e) => {
            println!("{:?}", e);
            return None;
        }
    };
}

// This should obviously return deserialized result.
fn get_csv_reader_iter(file_path: String) -> csv::Reader<fs::File> {
    return csv::Reader::from_path(file_path).unwrap();
}
