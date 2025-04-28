

use std::fmt;
use cynic::*;
use gitlab_schema::gitlab::{self as schema};
use gitlab_schema::DateTimeString;
use gitlab_schema::IdString;
use crate::PageInfo;
use crate::Namespace;

use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;


#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct Project {
    pub id: IdString,
    pub name: String,
    pub full_path: IdString,
    pub description: Option<String>,
    pub created_at: Option<DateTimeString>,
    pub namespace: Option<Namespace>,
    pub last_activity_at: Option<DateTimeString>,
}

#[derive(cynic::QueryVariables, Clone, Deserialize, Serialize, rkyv::Archive)]
pub struct SingleProjectQueryArguments {
    pub full_path: IdString
}

/// A lighter query to just get a project's pipelines
#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab", graphql_type = "Project")]
pub struct ProjectPipelineFragment {
    pub pipelines: Option<PipelineConnection>
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab", graphql_type = "Query", variables = "SingleProjectQueryArguments")]
pub struct ProjectPipelineQuery {
    #[arguments(fullPath: $full_path)]
    pub project: Option<ProjectPipelineFragment>
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab")]
pub struct ProjectEdge {
    pub cursor: String,
    pub node: Option<Project>,
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct ProjectConnection {
    pub count: i32, //Int! 	Total count of collection.
    pub edges: Option<Vec<Option<ProjectEdge>>>,
    pub nodes: Option<Vec<Option<Project>>>, //[UserCore] 	A list of nodes.
    pub page_info: PageInfo,                 // 	PageInfo! 	Information to aid in pagination.
}



#[derive(cynic::QueryVariables, Debug, Clone)]
pub struct MultiProjectQueryArguments {
    pub membership: Option<bool>,
    pub search: Option<String>,
    pub search_namespaces: Option<bool>,
    pub topics: Option<Vec<String>>,
    pub personal: Option<bool>,
    pub sort: String,
    pub ids: Option<Vec<IdString>>,
    pub full_paths: Option<Vec<String>>,
    pub with_issues_enabled: Option<bool>,
    pub with_merge_requests_enabled: Option<bool>,
    pub aimed_for_deletion: Option<bool>,
    pub include_hidden: Option<bool>,
    pub marked_for_deletion_on: Option<DateTimeString>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub first: Option<i32>,
    pub last: Option<i32>,
}

#[derive(cynic::QueryFragment, Clone)]
#[cynic(
    schema = "gitlab",
    graphql_type = "Query",
    variables = "MultiProjectQueryArguments"
)]
pub struct MultiProjectQuery {
    pub projects: Option<ProjectConnection>,
}

#[derive(cynic::Enum, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub enum AccessLevelEnum {
    NoAccess,
    MinimalAccess,
    Guest,
    Reporter,
    Developer,
    Maintainer,
    Owner,
    Admin,
}

impl fmt::Display for AccessLevelEnum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            AccessLevelEnum::NoAccess => "No Access",
            AccessLevelEnum::MinimalAccess => "Minimal Access",
            AccessLevelEnum::Guest => "Guest",
            AccessLevelEnum::Reporter => "Reporter",
            AccessLevelEnum::Developer => "Developer",
            AccessLevelEnum::Maintainer => "Maintainer",
            AccessLevelEnum::Owner => "Owner",
            AccessLevelEnum::Admin => "Admin",
        };
        write!(f, "{}", s)
    }
}

//
// Represents a Project Membership, and can be used as attributes for our relationships if supported in our graph db
//
#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub struct ProjectMember {
    // pub id: IdString,
    pub access_level: Option<crate::AccessLevel>,
    pub created_at: Option<DateTimeString>,
    // pub created_by: Option<UserCore>,
    pub expires_at: Option<DateTimeString>,
    pub updated_at: Option<DateTimeString>,
    pub project: Option<Project>,
    // pub user: Option<UserCore>,
    // pub user_permissions: ProjectPermissions,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub struct ProjectMemberEdge {
    cursor: String,
    node: Option<ProjectMember>,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub struct ProjectMemberConnection {
    pub edges: Option<Vec<Option<ProjectMemberEdge>>>,
    pub nodes: Option<Vec<Option<ProjectMember>>>,
    pub page_info: PageInfo,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub struct PipelineConnection {  
  pub nodes: Option<Vec<Option<Pipeline>>>,
  pub pageInfo: PageInfo
}

/// A shallow pipeline query fragment, gets pipeline metadata
#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub struct Pipeline {
    pub id: IdString,
    pub active: bool,
    pub sha: Option<String>,
    pub child: bool,
    // pub commit: Option<GitCommit>,
    pub complete: bool,
    pub compute_minutes: Option<f64>,
    pub created_at: DateTimeString,
    // pub downstream: Option<PipelineConnection>,
    pub duration: Option<i32>,
    pub failure_reason: Option<String>,
    pub finished_at: Option<DateTimeString>,
    // pub job_artifacts: Option<CiJobArtifact>,
    pub latest: bool,
    // pub status: PipelineStatusEnum,
    pub trigger: bool,
    pub total_jobs: i32,
}
