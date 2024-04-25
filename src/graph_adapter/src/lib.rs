/*
Polar (OSS)

Copyright 2024 Carnegie Mellon University.

NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for full terms.

[DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited distribution.  Please see Copyright notice for non-US Government use and distribution.

This Software includes and/or makes use of Third-Party Software each subject to its own license.

DM24-0470
*/

use async_trait::async_trait;
use serde::Deserialize;

#[derive(Deserialize,Clone)]
pub enum DriverType {
    NeoDriver,
}

// It sort of depends on the database connector, but some connectors may have
// the port as part of the URI, some separate it. I'm leaving us options that
// avoid parsing later. We might want to create fields for protocols, too.
#[derive(Deserialize,Clone)]
pub struct Connection {
    pub uri: String,
    pub port: String,
    pub user: String,
    pub pass: String,
    pub db: String,
}

#[derive(Deserialize,Clone)]
pub struct DriverConfig {
    pub driver_config: DriverType,
    pub connection: Connection,
}

pub trait GraphQuery<T> {
    fn create(create_str: String) -> T;
}

pub trait GraphConfig<S> {
    fn create(driver_conf: DriverConfig) -> S;
}
#[async_trait]
pub trait GraphDriver<'a,S,T,E,W> {
    async fn execute(& mut self) -> 
        Result<T, E>;

    async fn connect(query: T, config: W) -> S;
}
