/*
Polar (OSS)

Copyright 2024 Carnegie Mellon University.

NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
TRADEMARK, OR COPYRIGHT INFRINGEMENT.

Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
full terms.

[DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
distribution.  Please see Copyright notice for non-US Government use and distribution.

This Software includes and/or makes use of Third-Party Software each subject to its own license.

DM24-0470
*/


use std::{env, fs::{File, self}, io::{Read, Write}};
use std::process;
use url::Url;
use lapin::{Connection,ConnectionProperties, Channel, BasicProperties, publisher_confirm::Confirmation, options::BasicPublishOptions};
use tcp_stream::OwnedTLSConfig;
use sysinfo::{System, SystemExt, ProcessRefreshKind, Pid};
use log::{debug, error, info, warn};

pub const GITLAB_EXCHANGE_STR: &str = "gitlab_exchange";

pub const PROJECTS_ROUTING_KEY: &str = "projects";
pub const PROJECTS_QUEUE_NAME : &str = "gitlab_projects";

pub const GROUPS_ROUTING_KEY: &str = "groups";
pub const GROUPS_QUEUE_NAME: &str = "gitlab_groups";

pub const USERS_QUEUE_NAME: &str = "users";
pub const USERS_ROUTING_KEY: &str = "gitlab_users";

pub const RUNNERS_QUEUE_NAME: &str = "runners";
pub const RUNNERS_ROUTING_KEY: &str = "gitlab_runners";

// Checks for the existence of a lock file at the given path. Creates lock file if not found.
pub fn create_lock(filepath: &str) -> Result<bool, std::io::Error> {
    let file_path = std::path::Path::new(filepath);
    let exists = file_path.exists();
    if exists == true {
        // Check if there is a PID in the file and check if that PID is actually
        // running. If it is running, we're done here. If it's not running, we
        // need to delete the file. Regardless of whether or not this condition
        // is true, we need to unconditionally create a new lock file with our
        // new PID, at that point.
        if let Ok(mut handle) = File::open(file_path) {
            let mut bytes_buf = [0u8; 4];
            let bytes_read = handle.read(&mut bytes_buf).unwrap();
            if bytes_read == 4 {
                let my_pid = u32::from_le_bytes(bytes_buf);

                let mut s = System::new_all();
                let result = s.refresh_process_specifics(Pid::from(usize::try_from(my_pid).unwrap()), ProcessRefreshKind::new());
                if result {
                    // println!("[*] An instance of this observer is still running. No further scheduler action taken at this time. Yielding.");
                    return Ok(false)
                } else {
                    // println!("[*] A lock file was found, but the PID was invalid. Deleting lock file and creating a new one.");
                    fs::remove_file(file_path)?;
                }
            } else {
                panic!("Lock file contains bad data.")
            }
        } else {
            panic!("We found a lock file but couldn't open it.")
        }
    }

    // Create lock file unconditionally, return false
    let new_handle = File::create(filepath);
    match new_handle {
        Ok(mut new_handle) => {
            // Get current PID.
            let pid = process::id();
            let new_pid = u32::to_le_bytes(pid);
            let write_result = new_handle.write_all(&new_pid);
            match write_result {
                Ok(_) => println!("New lock file created with PID: {}", pid),
                Err(e) => {
                    println!("Failed to create new lock file.");
                    return Err(e)
                }
            }
            return Ok(true)
        }
        Err(e) => {
            panic!("[*] Problem creating lock file: {}", e.to_string());
        }
    }
}

pub fn get_gitlab_token() -> String {
    let token = read_from_env("GITLAB_TOKEN".to_owned());
    //check length and prefix
    if token.chars().count() == 26 && token.starts_with("glpat-") {
        return token;
    }else {
        error!("received invalid private token from environment.");
        process::exit(1)
    }
}

pub fn get_gitlab_endpoint()-> String {
    //TODO: Check validity of service endpoint url loaded from env
    //verify URL is a valid format
    let endpoint = read_from_env("GITLAB_ENDPOINT".to_owned());
    match Url::parse(endpoint.as_str()) {
        Ok(url) => {
            //TODO: confirm url further?
            return url.to_string()
        }
        Err(e) => {
            error!("error parsing Gitlab Endpoint read from environment, {}", e);
            process::exit(1)
        }
    }
}

fn get_file_as_byte_vec(filename: &String) -> Vec<u8> {
    let mut f = File::open(&filename).expect("no file found");
    let metadata = std::fs::metadata(&filename).expect("unable to read metadata");
    let mut buffer = vec![0; metadata.len() as usize];
    f.read(&mut buffer).expect("buffer overflow");

    buffer
}

/// Get's a connection to rabbitmq using mutual TLS and no credentials, EXTERNAL authentication mechanisms
/// Ensure valid certificates are present
pub async fn connect_to_rabbitmq() -> Result<Connection, String> {
    // You need to use amqp:// scheme here to handle the TLS part manually as it's automatic when you use amqps://
    let rabbit_endpoint = env::var("BROKER_ENDPOINT").unwrap_or_else( |_| {
        //TODO: alert user via logs that endpoint wasn't loaded from config, exit.
        
        error!("Could not load rabbitmq instance endpoint from environment.");
        process::exit(1)
    });
    let cert_chain = env::var("TLS_CA_CERT").unwrap_or_else(|_| {
        error!("Could not locate TLS_CA_CERT using path from environment.");
        process::exit(1)
    });

    //configure uri auth mechanism
    let client_key_file= env::var("TLS_CLIENT_KEY").unwrap_or_else(|_| {
        error!("Could not read TLS_CLIENT_KEY using path from environment.");
        process::exit(1)
    });
    let client_key_pwd = env::var("TLS_KEY_PASSWORD").unwrap_or_else(|_| {
        error!("Could not read TLS_KEY_PASSWORD from environment.");
        process::exit(1)
     });

   let tls_config = OwnedTLSConfig {
        identity: Some(tcp_stream::OwnedIdentity {
            der: get_file_as_byte_vec(&client_key_file),
            password: client_key_pwd
        }),
        cert_chain: Some(std::fs::read_to_string(cert_chain).unwrap())

    };

   info!("connecting to: {}", rabbit_endpoint);

   // println!("rabbit endpoint: {}", &rabbit_endpoint);
   // println!("TLS config: {:?}", tls_config);
   let conn = Connection::connect_with_config(&rabbit_endpoint, ConnectionProperties::default() ,tls_config).await.unwrap_or_else(|_| {
    error!("Could not connect to rabbitmq at {}", rabbit_endpoint);
    process::exit(1)
   });

   Ok(conn)
}

/// Publish a message to the rabbitmq instance at a given exchange, using the channel and routing key for a desired queue.
pub async fn publish_message(payload: &[u8], channel: &Channel, exchange: &str, routing_key: &str){
    
    let confirmation = channel.basic_publish(exchange, routing_key, 
    BasicPublishOptions::default(),
        payload,
        BasicProperties::default()).await.unwrap().await.unwrap();
    
    assert_eq!(confirmation, Confirmation::NotRequested);
}

//TODO: Review this fn, do we always want to exit when the environment isn't fully configured? Are any env vars optional?
pub fn read_from_env(var_name: String) -> String {
    match env::var(var_name.clone()) {
        Ok(val) => val,
        Err(e) => {
            error!("Can't read {} from environment", var_name);
            process::exit(1)
        }
    }
}