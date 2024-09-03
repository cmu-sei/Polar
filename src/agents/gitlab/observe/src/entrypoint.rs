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

use std::{env, process};
use std::{error::Error, time::Duration};
use std::process::Command;
use std::path::Path;
use std::sync::atomic::{AtomicBool,Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use common::GITLAB_EXCHANGE_STR;
use clokwerk::{Interval::{self}, Scheduler};
use ctrlc;
use lapin::{options::ExchangeDeclareOptions,types::FieldTable};
use log::{error, info};
use serde_yaml::Value;

enum ChildMessage {
    Terminate,
}

fn get_scheduler_config(config_filepath: String) -> Value {
    if let true =  Path::new(config_filepath.as_str()).exists() {
        let config_str = std::fs::read_to_string(config_filepath).unwrap();
        let config: Value = serde_yaml::from_str(&config_str).unwrap();
        return config
    }
    else {
        panic!("File path read from environment (GITLAB_OBSERVER_CONFIG) does not exist, {}", config_filepath);
    }

}

fn schedule_observers(config: Value, tx: mpsc::Sender<ChildMessage>, ctrl_c: Arc<AtomicBool>) {
    let mut scheduler = Scheduler::new();
    //add tasks based off of config
    let resources = config["resources"].as_sequence().unwrap().to_owned();

    for resource in resources {
        match resource {
            Value::Mapping(mapping) => {
                // Get resource type from value of mapping.
                let map = mapping.clone();
                let val = map.keys().next().unwrap().to_owned();
                let resource_type: String = serde_yaml::from_value(val.clone()).unwrap();
                // Schedule based on resource type and frequency
                let frequency = mapping.get(resource_type.clone()).unwrap()
                .get("frequency").unwrap()
                .as_u64().unwrap();
                
                if resource_type == "projects" {
                    let my_tx = tx.clone();
                    let my_ctrl_c = ctrl_c.clone();
                    scheduler.every(Interval::Seconds(frequency.try_into().unwrap())).run( move || {
                        let mut command = Command::new("./gitlab_projects").spawn().expect("Could not execute gitlab projects binary.");

                        loop {
                            if my_ctrl_c.load(Ordering::Relaxed) {
                                break;
                            }

                            match command.try_wait() {
                                Ok(Some(_)) => break,
                                _ => {}
                            }
                        }
                        my_tx.send(ChildMessage::Terminate).expect("Failed to send termination message.");
                    });
                } else if resource_type == "users" {
                    let my_tx = tx.clone();
                    let my_ctrl_c = ctrl_c.clone();
                    scheduler.every(Interval::Seconds(frequency.try_into().unwrap())).run( move || {
                        let mut command = Command::new("./gitlab_users").spawn().expect("Could not execute gitlab users binary.");

                        loop {
                            if my_ctrl_c.load(Ordering::Relaxed) {
                                break;
                            }

                            match command.try_wait() {
                                Ok(Some(_)) => break,
                                _ => {}
                            }
                        }
                        my_tx.send(ChildMessage::Terminate).expect("Failed to send termination message.");
                    });
                } else if resource_type == "groups" {
                    let my_tx = tx.clone();
                    let my_ctrl_c = ctrl_c.clone();
                    scheduler.every(Interval::Seconds(frequency.try_into().unwrap())).run( move || {
                        let mut command = Command::new("./gitlab_groups").spawn().expect("Could not execute gitlab groups binary.");

                        loop {
                            if my_ctrl_c.load(Ordering::Relaxed) {
                                break;
                            }

                            match command.try_wait() {
                                Ok(Some(_)) => break,
                                _ => {}
                            }
                        }
                        my_tx.send(ChildMessage::Terminate).expect("Failed to send termination message.");
                    });
                } else if resource_type == "runners" {
                    let my_tx = tx.clone();
                    let my_ctrl_c = ctrl_c.clone();
                    scheduler.every(Interval::Seconds(frequency.try_into().unwrap())).run( move || {
                            let mut command = Command::new("./gitlab_runners").spawn().expect("Could not execute gitlab runners.");

                            loop {
                                if my_ctrl_c.load(Ordering::Relaxed) {
                                    break;
                                }

                                match command.try_wait() {
                                    Ok(Some(_)) => break,
                                    _ => {}
                                }
                            }
                            my_tx.send(ChildMessage::Terminate).expect("Failed to send termination message.");
                    });
                }
            }
            _ => todo!()
        }
    }
    tokio::spawn(async move {
        loop {
          scheduler.run_pending();
          thread::sleep(Duration::from_millis(100));
        }
    });
}

async fn setup_rabbitmq() -> Result<(), lapin::Error> {
    let mq_conn = common::connect_to_rabbitmq().await.unwrap();
    
    // Create publish channel and exchange
    let mq_publish_channel = mq_conn.create_channel().await?;
    mq_publish_channel.exchange_declare(GITLAB_EXCHANGE_STR, lapin::ExchangeKind::Direct, 
    ExchangeDeclareOptions::default(),FieldTable::default()).await?;

    info!("[*] Gitlab Exchange Declared");

    let _ = mq_conn.close(0, "closed").await?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error> > {
    env_logger::init();
    
    setup_rabbitmq().await?;

    //schedule and fire off observers
    let config_path = env::var("GITLAB_OBSERVER_CONFIG").unwrap_or_else(|_| {
        error!("Could not find the gitlab observer scheduler configuration variable, (GITLAB_OBSERVER_CONFIG) in the runtime environment. Please ensure the location of this file has been set in the named variable.");
        process::exit(1)
    });

    let (tx, rx) = mpsc::channel();
    let ctrl_c = Arc::new(AtomicBool::new(false));

    let config = get_scheduler_config(config_path);

    schedule_observers(config, tx.clone(), ctrl_c.clone());

    // Handle the CTRL-C signal
    let ctrl_c_signal = Arc::clone(&ctrl_c);
    ctrlc::set_handler(move || {
        ctrl_c_signal.store(true, Ordering::Relaxed);
    })
    .expect("Failed to set CTRL-C handler");
    
    // Wait for the CTRL-C signal
    while !ctrl_c.load(Ordering::Relaxed) {
        thread::yield_now();
    }

    tx.send(ChildMessage::Terminate).expect("Failed to send termination message");

    // Block until at least one child process has exited.
    for _ in 0..1 {
        rx.recv().unwrap();
        thread::sleep(Duration::from_secs(1));
    }
    
    Ok(())
}
