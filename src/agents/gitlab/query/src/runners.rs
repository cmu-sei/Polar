use gitlab_schema::{gitlab::{self as schema}, DateTimeString, IdString};
use crate::UserCoreFragment;
use std::fmt;
use rkyv::Archive;
use rkyv::Serialize;
use rkyv::Deserialize;


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
pub struct PageInfo {
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub start_cursor: Option<String>,
}


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
    pub created_by: Option<UserCoreFragment>,
    pub tag_list: Option<Vec<String>>
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
    Paused
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
    
    pub creator_id: Option<IdString>    
}

#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab", graphql_type = "Query" , variables = "MultiRunnerQueryArguments")]
pub struct MultiRunnerQuery {
    // #[arguments(fullPath: $full_path)]
    pub runners: Option<CiRunnerConnection>
}