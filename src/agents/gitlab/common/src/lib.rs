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

use std::{fs::File, io::Read};
pub mod dispatch;
pub mod types;

// TODO: These are suitable for one particular instance, but eventually we may have others, what'll be a good way
// to be able to make sure queues don't get confused between instances?
pub const METADATA_CONSUMER_TOPIC: &str = "gitlab.consumer.metadata";
pub const PROJECTS_CONSUMER_TOPIC: &str = "gitlab:consumer:projects";
pub const GROUPS_CONSUMER_TOPIC: &str = "gitlab:consumer:groups";
pub const USER_CONSUMER_TOPIC: &str = "gitlab:consumer:users";
pub const RUNNERS_CONSUMER_TOPIC: &str = "gitlab:consumer:runners";
pub const PIPELINE_CONSUMER_TOPIC: &str = "gitlab:consumer:pipelines";
pub const REPOSITORY_CONSUMER_TOPIC: &str = "gitlab:consumer:repositories";

/// Helper function to parse a file at a given path and return the raw bytes as a vector
pub fn get_file_as_byte_vec(filename: &String) -> Result<Vec<u8>, std::io::Error> {
    let mut f = File::open(&filename)?;

    let metadata = std::fs::metadata(&filename)?;

    let mut buffer = vec![0; metadata.len() as usize];
    f.read(&mut buffer).expect("buffer overflow");

    Ok(buffer)
}
