/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

use std::fmt;

use cynic::*;
use gitlab_schema::gitlab::{self as schema};
use gitlab_schema::DateTimeString;
use gitlab_schema::IdString;
use projects::AccessLevelEnum;
use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;

pub mod users;
pub mod groups;
pub mod projects;
pub mod runners;

// #[derive(cynic::Scalar,serde::Deserialize, Clone, Debug)]
// #[cynic(schema = "gitlab", graphql_type = "UserID")]
// pub struct UserID(Id);


#[derive(cynic::QueryFragment, Debug, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct Namespace {
    pub id: IdString,
    pub full_name: String,
    pub full_path: IdString,
    pub visibility: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct PageInfo {
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub start_cursor: Option<String>,
}


#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub struct AccessLevel {
    pub integer_value: Option<i32>,
    pub string_value: Option<AccessLevelEnum>,
}

