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

use std::{error::Error, time::Duration};
use std::{env, thread};
use gitlab_observer::*;
use ractor::Actor;
use polar::init_logging;

// enum ChildMessage {
//     Terminate,
// }

/// Helper to parse and validate the observer configuration
/// TODO: Unfreeze and leverage within a potential configuration supervisor that will manage observer agents
// fn get_scheduler_config(config_filepath: String) -> Value {
//     if let true =  Path::new(config_filepath.as_str()).exists() {
//         let config_str = std::fs::read_to_string(config_filepath).unwrap();
//         let config: Value = serde_yaml::from_str(&config_str).unwrap();
//         return config
//     }
//     else {
//         panic!("File path read from environment (GITLAB_OBSERVER_CONFIG) does not exist, {}", config_filepath);
//     }

// }
/// Helper fn to schedule observers based off of our given configuration,
/// TODO: Unfreeze this function and implement it within thje observer supervisor. refactor as needed to make more general for all observer agents
// fn schedule_observers(config: Value, tx: mpsc::Sender<ChildMessage>, ctrl_c: Arc<AtomicBool>) {
//     let mut scheduler = Scheduler::new();
//     //add tasks based off of config
//     let resources = config["resources"].as_sequence().unwrap().to_owned();

//     for resource in resources {
//         match resource {
//             Value::Mapping(mapping) => {
//                 // Get resource type from value of mapping.
//                 let map = mapping.clone();
//                 let val = map.keys().next().unwrap().to_owned();
//                 let resource_type: String = serde_yaml::from_value(val.clone()).unwrap();
//                 // Schedule based on resource type and frequency
//                 let frequency = mapping.get(resource_type.clone()).unwrap()
//                 .get("frequency").unwrap()
//                 .as_u64().unwrap();
                
//                 if resource_type == "projects" {
//                     let my_tx = tx.clone();
//                     let my_ctrl_c = ctrl_c.clone();
//                     scheduler.every(Interval::Seconds(frequency.try_into().unwrap())).run( move || {
//                         let mut command = Command::new("./gitlab_projects").spawn().expect("Could not execute gitlab projects binary.");

//                         loop {
//                             if my_ctrl_c.load(Ordering::Relaxed) {
//                                 break;
//                             }

//                             match command.try_wait() {
//                                 Ok(Some(_)) => break,
//                                 _ => {}
//                             }
//                         }
//                         my_tx.send(ChildMessage::Terminate).expect("Failed to send termination message.");
//                     });
//                 } else if resource_type == "users" {
//                     let my_tx = tx.clone();
//                     let my_ctrl_c = ctrl_c.clone();
//                     scheduler.every(Interval::Seconds(frequency.try_into().unwrap())).run( move || {
//                         let mut command = Command::new("./gitlab_users").spawn().expect("Could not execute gitlab users binary.");

//                         loop {
//                             if my_ctrl_c.load(Ordering::Relaxed) {
//                                 break;
//                             }

//                             match command.try_wait() {
//                                 Ok(Some(_)) => break,
//                                 _ => {}
//                             }
//                         }
//                         my_tx.send(ChildMessage::Terminate).expect("Failed to send termination message.");
//                     });
//                 } else if resource_type == "groups" {
//                     let my_tx = tx.clone();
//                     let my_ctrl_c = ctrl_c.clone();
//                     scheduler.every(Interval::Seconds(frequency.try_into().unwrap())).run( move || {
//                         let mut command = Command::new("./gitlab_groups").spawn().expect("Could not execute gitlab groups binary.");

//                         loop {
//                             if my_ctrl_c.load(Ordering::Relaxed) {
//                                 break;
//                             }

//                             match command.try_wait() {
//                                 Ok(Some(_)) => break,
//                                 _ => {}
//                             }
//                         }
//                         my_tx.send(ChildMessage::Terminate).expect("Failed to send termination message.");
//                     });
//                 } else if resource_type == "runners" {
//                     let my_tx = tx.clone();
//                     let my_ctrl_c = ctrl_c.clone();
//                     scheduler.every(Interval::Seconds(frequency.try_into().unwrap())).run( move || {
//                             let mut command = Command::new("./gitlab_runners").spawn().expect("Could not execute gitlab runners.");

//                             loop {
//                                 if my_ctrl_c.load(Ordering::Relaxed) {
//                                     break;
//                                 }

//                                 match command.try_wait() {
//                                     Ok(Some(_)) => break,
//                                     _ => {}
//                                 }
//                             }
//                             my_tx.send(ChildMessage::Terminate).expect("Failed to send termination message.");
//                     });
//                 }
//             }
//             _ => todo!()
//         }
//     }
//     tokio::spawn(async move {
//         loop {
//           scheduler.run_pending();
//           thread::sleep(Duration::from_millis(100));
//         }
//     });
// }


#[tokio::main]
async fn main() -> Result<(), Box<dyn Error> > {
    init_logging();
    
    
    // let config_path = read_from_env("GITLAB_OBSERVER_CONFIG".to_owned());

    // let config = get_scheduler_config(config_path);
    // debug!("using provided config:\n {:#?}", config);

    // info!("Scheduling observers");
    // TODO: Pass in values from config through args  
    let client_cert_file = env::var("TLS_CLIENT_CERT").unwrap();
    let client_private_key_file = env::var("TLS_CLIENT_KEY").unwrap();
    let ca_cert_file =  env::var("TLS_CA_CERT").unwrap();   
    
    let gitlab_endpoint = env::var("GITLAB_ENDPOINT").unwrap();
    let broker_addr = env::var("BROKER_ADDR").unwrap();
    let gitlab_token = env::var("GITLAB_TOKEN").unwrap();

    let args = supervisor::ObserverSupervisorArgs {
        broker_addr,
        client_cert_file,
        client_private_key_file,
        ca_cert_file: ca_cert_file,
        gitlab_endpoint,
        gitlab_token: Some(gitlab_token),
    };

    let (supervisor, handle) = Actor::spawn(Some("GITLAB_OBSERVER_SUPERVISOR".to_string()), supervisor::ObserverSupervisor,args).await.expect("Expected to start observer agent");
    let _ = handle.await;

    
    Ok(())
}
