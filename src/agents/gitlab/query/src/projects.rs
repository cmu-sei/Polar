

use std::fmt;
use gitlab_schema::gitlab::{self as schema};
use gitlab_schema::{CiJobArtifactID, DateTimeString};
use gitlab_schema::IdString;
use gitlab_schema::JobIdString;
use crate::runners::CiRunner;
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
  pub page_info: PageInfo
}
#[derive(cynic::Enum, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub enum CiJobStatus {
    Created,
    WaitingForResource,
    Preparing,
    WaitingForCallback,
    Pending,
    Running,
    Success,
    Failed,
    Canceling,
    Canceled,
    Skipped,
    Manual,
    Scheduled
}

#[derive(cynic::Enum, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
enum JobArtifactFileType {
    Archive,
    Metadata,
    Trace,
    Junit,
    Metrics,
    MetricsReferee,
    Lsif,
    Cyclonedx,
    Annotations,
    RepositoryXray,
    Sast,
    SecretDetection,
    DependencyScanning,
    ContainerScanning,
    ClusterImageScanning,
    Dast,
    LicenseScanning,
    Accessibility,
    Codequality,
    Performance,
    BrowserPerformance,
    Terraform,
    Requirements,
    RequirementsV2,
    CoverageFuzzing,
    ApiFuzzing,
    ClusterApplications, Cobertura, Dotenv, LoadPerformance, NetworkReferee
}

/// A shallow pipeline query fragment, gets pipeline metadata
#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub struct Pipeline {
    pub id: IdString,
    pub active: bool,
    pub sha: Option<String>,
    pub child: bool,
    pub commit: Option<Commit>,
    pub complete: bool,
    pub compute_minutes: Option<f64>,
    pub created_at: DateTimeString,
    // pub downstream: Option<PipelineConnection>,
    pub duration: Option<i32>,
    pub failure_reason: Option<String>,
    pub finished_at: Option<DateTimeString>,
    pub job_artifacts: Option<Vec<CiJobArtifact>>,
    pub latest: bool,
    // pub status: PipelineStatusEnum,
    pub trigger: bool,
    pub total_jobs: i32,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab", graphql_type = "CiJob")]
pub struct GitlabCiJob {
    pub id: Option<JobIdString>,
    pub status: Option<CiJobStatus>,
    pub runner: Option<CiRunner>,
    pub name: Option<String>,
    pub short_sha: String,
    pub tags: Option<Vec<String>>,
    // pub kind: CiJobKind,
    pub created_at: Option<DateTimeString>,
    pub started_at: Option<DateTimeString>,
    pub finished_at: Option<DateTimeString>,
    pub duration: Option<i32>, 
    pub failure_message: Option<String>,
    pub artifacts: Option<CiJobArtifactConnection>
}


#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(
    schema = "gitlab",
    graphql_type = "Commit"
)]
pub struct Commit {
    pub author: Option<crate::users::UserCoreFragment>,
    pub author_email: Option<String>,
    pub author_gravatar: Option<String>,
    pub author_name: Option<String>,
    pub authored_date: Option<DateTimeString>,
    pub committed_date: Option<DateTimeString>,
    pub committer_email: Option<String>,
    pub committer_name: Option<String>,
    pub description: Option<String>,
    pub description_html: Option<String>,
    // pub diffs: Option<Vec<Diff>>,
    pub full_title: Option<String>,
    pub full_title_html: Option<String>,
    pub id: IdString,
    pub message: Option<String>,
    pub sha: String,
    pub short_id: String,
    // pub signature: Option<CommitSignature>,
    pub signature_html: Option<String>,
    pub title: Option<String>,
    pub title_html: Option<String>,
    pub web_path: String,
    pub web_url: String,
}
#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub struct CiJobArtifact {
    pub download_path: Option<String>,
    pub expire_at: Option<DateTimeString>,
    pub file_type: Option<JobArtifactFileType>,
    pub id: CiJobArtifactID,
    pub name: Option<String>,
    pub size: gitlab_schema::BigInt,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub struct CiJobArtifactConnection {  
  pub nodes: Option<Vec<Option<CiJobArtifact>>>,
  pub pageInfo: PageInfo
}
