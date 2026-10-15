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

use gitlab_schema::{
    DateTimeString, IdString,
    gitlab::{self as schema},
};
use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;

#[derive(cynic::QueryFragment, Deserialize, Serialize, Archive, Clone)]
pub struct CiRunner {
    pub access_level: CiRunnerAccessLevel,
    pub active: bool,
    pub id: CiRunnerID,
    pub paused: bool,
    pub run_untagged: bool,
    pub runner_type: CiRunnerType,
    pub status: CiRunnerStatus,
    pub contacted_at: Option<DateTimeString>,
    pub created_at: Option<DateTimeString>,
    pub created_by: Option<crate::users::UserCoreFragment>,
    pub tag_list: Option<Vec<String>>,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, Archive, Clone)]
#[cynic(graphql_type = "CiRunner")]
pub struct CiRunnerIdFragment {
    pub id: CiRunnerID,
}
#[derive(cynic::QueryFragment, Deserialize, Serialize, Archive, Clone)]
pub struct CiRunnerConnection {
    pub edges: Option<Vec<Option<CiRunnerEdge>>>,
    pub nodes: Option<Vec<Option<CiRunner>>>,
    pub page_info: PageInfo,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, Archive, Clone)]
pub struct CiRunnerEdge {
    pub cursor: String,
    pub node: Option<CiRunner>,
}
#[derive(cynic::QueryFragment, Deserialize, Serialize, Archive, Clone)]
#[cynic(graphql_type = "CiRunnerEdge")]
pub struct CiRunnerIdEdge {
    pub cursor: String,
    pub node: Option<CiRunnerIdFragment>,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, Archive, Clone)]
#[cynic(graphql_type = "CiRunnerConnection")]
pub struct CiRunnerIdConnection {
    pub edges: Option<Vec<Option<CiRunnerIdEdge>>>,
    pub nodes: Option<Vec<Option<CiRunnerIdFragment>>>,
    pub page_info: PageInfo,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, Archive, Clone)]
pub struct PageInfo {
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub start_cursor: Option<String>,
}

#[derive(cynic::Enum, Clone, Copy, Debug, Deserialize, Serialize, Archive)]
pub enum CiRunnerAccessLevel {
    NotProtected,
    RefProtected,
}

#[derive(cynic::Scalar, Clone, Deserialize, Serialize, Archive)]
pub struct CiRunnerID(pub String);

#[derive(cynic::Enum, Clone, Debug, Deserialize, Serialize, Archive)]
pub enum CiRunnerType {
    InstanceType,
    GroupType,
    ProjectType,
}

#[derive(cynic::Enum, Clone, Debug, Deserialize, Serialize, Archive)]
pub enum CiRunnerStatus {
    Online,
    Offline,
    Stale,
    Active,
    NeverContacted,
    Paused,
}

#[derive(cynic::QueryVariables, Debug, Deserialize, Serialize, Archive)]
pub struct MultiRunnerQueryArguments {
    pub paused: Option<bool>,

    pub status: Option<CiRunnerStatus>,

    // runner_type: CiRunnerType,
    pub tag_list: Option<Vec<String>>,

    pub search: Option<String>,

    // sort: CiRunnerSort,

    // upgradeStatus: CiRunnerUpgradeStatus,
    pub creator_id: Option<IdString>,
}

#[derive(cynic::QueryFragment)]
#[cynic(
    schema = "gitlab",
    graphql_type = "Query",
    variables = "MultiRunnerQueryArguments"
)]
pub struct MultiRunnerQuery {
    // #[arguments(fullPath: $full_path)]
    pub runners: Option<CiRunnerConnection>,
}
