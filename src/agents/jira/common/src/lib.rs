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

//use lapin::{
//    options::BasicPublishOptions, publisher_confirm::Confirmation, BasicProperties, Channel,
//    Connection, ConnectionProperties,
//};
use std::process;
use std::{
//    env,
    fs::{self, File},
    io::{Read, Write},
};
use sysinfo::{Pid, ProcessRefreshKind, System, SystemExt};
//use tcp_stream::OwnedTLSConfig;
//use tracing::{error, info};
//use url::Url;

pub mod dispatch;
pub mod types;

pub const JIRA_PROJECTS_CONSUMER_TOPIC: &str = "jira:consumer:projects";

// Checks for the existence of a lock file at the given path. Creates lock file if not found.
#[deprecated]
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
                let result = s.refresh_process_specifics(
                    Pid::from(usize::try_from(my_pid).unwrap()),
                    ProcessRefreshKind::new(),
                );
                if result {
                    // println!("[*] An instance of this observer is still running. No further scheduler action taken at this time. Yielding.");
                    return Ok(false);
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
                    return Err(e);
                }
            }
            return Ok(true);
        }
        Err(e) => {
            panic!("[*] Problem creating lock file: {}", e.to_string());
        }
    }
}

/// Helper function to parse a file at a given path and return the raw bytes as a vector
pub fn get_file_as_byte_vec(filename: &String) -> Result<Vec<u8>, std::io::Error> {
    let mut f = File::open(&filename)?;

    let metadata = std::fs::metadata(&filename)?;

    let mut buffer = vec![0; metadata.len() as usize];
    f.read(&mut buffer).expect("buffer overflow");

    Ok(buffer)
}
