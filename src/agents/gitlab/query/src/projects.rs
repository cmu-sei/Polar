

use std::fmt;
use gitlab_schema::gitlab::{self as schema};
use gitlab_schema::{CiJobArtifactID, DateTimeString};
use gitlab_schema::IdString;
use gitlab_schema::JobIdString;
use gitlab_schema::ContainerRepositoryID;
use gitlab_schema::BigInt;
use crate::runners::CiRunnerIdFragment;
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
#[cynic(schema = "gitlab", graphql_type = "Project")]
pub struct ProjectPipelineJobsragment {
    pub pipelines: Option<PipelineJobsConnection>
}


#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab", graphql_type = "Query", variables = "SingleProjectQueryArguments")]
pub struct ProjectPipelineQuery {
    #[arguments(fullPath: $full_path)]
    pub project: Option<ProjectPipelineFragment>
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab", graphql_type = "Query", variables = "SingleProjectQueryArguments")]
pub struct ProjectPipelineJobsQuery {
    #[arguments(fullPath: $full_path)]
    pub project: Option<ProjectPipelineJobsragment>
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

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab", graphql_type = "PipelineConnection")]
pub struct PipelineJobsConnection {  
  pub nodes: Option<Vec<Option<PipelineJobsFragment>>>,
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

impl fmt::Display for CiJobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status_str = match self {
            CiJobStatus::Created => "created",
            CiJobStatus::WaitingForResource => "waiting_for_resource",
            CiJobStatus::Preparing => "preparing",
            CiJobStatus::WaitingForCallback => "waiting_for_callback",
            CiJobStatus::Pending => "pending",
            CiJobStatus::Running => "running",
            CiJobStatus::Success => "success",
            CiJobStatus::Failed => "failed",
            CiJobStatus::Canceling => "canceling",
            CiJobStatus::Canceled => "canceled",
            CiJobStatus::Skipped => "skipped",
            CiJobStatus::Manual => "manual",
            CiJobStatus::Scheduled => "scheduled",
        };
        write!(f, "{}", status_str)
    }
}

#[derive(cynic::Enum, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub enum JobArtifactFileType {
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

impl fmt::Display for JobArtifactFileType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            JobArtifactFileType::Archive => "archive",
            JobArtifactFileType::Metadata => "metadata",
            JobArtifactFileType::Trace => "trace",
            JobArtifactFileType::Junit => "junit",
            JobArtifactFileType::Metrics => "metrics",
            JobArtifactFileType::MetricsReferee => "metrics_referee",
            JobArtifactFileType::Lsif => "lsif",
            JobArtifactFileType::Cyclonedx => "cyclonedx",
            JobArtifactFileType::Annotations => "annotations",
            JobArtifactFileType::RepositoryXray => "repository_xray",
            JobArtifactFileType::Sast => "sast",
            JobArtifactFileType::SecretDetection => "secret_detection",
            JobArtifactFileType::DependencyScanning => "dependency_scanning",
            JobArtifactFileType::ContainerScanning => "container_scanning",
            JobArtifactFileType::ClusterImageScanning => "cluster_image_scanning",
            JobArtifactFileType::Dast => "dast",
            JobArtifactFileType::LicenseScanning => "license_scanning",
            JobArtifactFileType::Accessibility => "accessibility",
            JobArtifactFileType::Codequality => "codequality",
            JobArtifactFileType::Performance => "performance",
            JobArtifactFileType::BrowserPerformance => "browser_performance",
            JobArtifactFileType::Terraform => "terraform",
            JobArtifactFileType::Requirements => "requirements",
            JobArtifactFileType::RequirementsV2 => "requirements_v2",
            JobArtifactFileType::CoverageFuzzing => "coverage_fuzzing",
            JobArtifactFileType::ApiFuzzing => "api_fuzzing",
            JobArtifactFileType::ClusterApplications => "cluster_applications",
            JobArtifactFileType::Cobertura => "cobertura",
            JobArtifactFileType::Dotenv => "dotenv",
            JobArtifactFileType::LoadPerformance => "load_performance",
            JobArtifactFileType::NetworkReferee => "network_referee",
        };
        write!(f, "{}", s)
    }
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab")]
pub struct CiJobConnection {
    pub nodes: Option<Vec<Option<GitlabCiJob>>>,
    pub page_info: PageInfo
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
    // TODO: show downstream pipeline connections
    //  pub downstream: Option<PipelineConnection>,
    pub duration: Option<i32>,
    pub failure_reason: Option<String>,
    pub finished_at: Option<DateTimeString>,
    pub job_artifacts: Option<Vec<CiJobArtifact>>,
    pub latest: bool,
    pub source: Option<String>,
    // TODO: pipeline statuses
    // pub status: PipelineStatusEnum,
    pub trigger: bool,
    pub total_jobs: i32,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab", graphql_type = "Pipeline")]
pub struct PipelineJobsFragment {
    pub id: IdString,
    pub jobs: Option<CiJobConnection>
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(schema = "gitlab", graphql_type = "CiJob")]
pub struct GitlabCiJob {
    pub id: Option<JobIdString>,
    pub status: Option<CiJobStatus>,
    
    pub runner: Option<CiRunnerIdFragment>,
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
  pub page_info: PageInfo
}

#[derive(cynic::Enum, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub enum ContainerRepositoryCleanupStatus {
    Unscheduled,
    Scheduled,
    Unfinished,
    Ongoing,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct ContainerRepository {
    pub id: IdString,
    pub created_at: DateTimeString,
    // pub expiration_policy_cleanup_status: Option<ContainerRepositoryCleanupStatus>,
    pub expiration_policy_started_at: Option<DateTimeString>,
    pub last_cleanup_deleted_tags_count: Option<i32>,
    pub location: String,
    pub migration_state: String,
    pub name: String,
    pub path: String,
    pub protection_rule_exists: bool,
    // pub status: Option<ContainerRepositoryStatus>,
    pub tags_count: i32,
    pub updated_at: DateTimeString,
    // pub user_permissions: ContainerRepositoryPermissions,
    // pub project: Project, // You should define a Project fragment separately
}

/// a struct representing an image tag
#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct ContainerRepositoryTag {
    pub created_at: Option<DateTimeString>,
    pub digest: Option<String>,
    pub location: String,
    pub media_type: Option<String>,
    pub name: String,
    pub path: String,
    pub published_at: Option<DateTimeString>,
    // pub referrers: Option<Vec<ContainerRepositoryReferrer>>,
    pub revision: Option<String>,
    pub short_revision: Option<String>,
    pub total_size: Option<BigInt>,
    // pub user_permissions: ContainerRepositoryTagPermissions,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive)]
pub struct ContainerRepositoryTagConnection {
    pub nodes: Option<Vec<Option<ContainerRepositoryTag>>>,
    pub page_info: PageInfo,
}

/// This is a sort-of redundant type, only really helpful for some additional metadata and
/// to get the connection between the repository and the underlying image tags
/// See gitlab's graphql docs
#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab", graphql_type = "ContainerRepositoryDetails")]
pub struct ContainerRepositoryTags {
    pub tags: Option<ContainerRepositoryTagConnection>
}

#[derive(cynic::QueryVariables, Deserialize, Serialize, rkyv::Archive)]
pub struct ContainerRepositoryDetailsArgs {
    pub id: ContainerRepositoryID
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab", graphql_type = "Query", variables = "ContainerRepositoryDetailsArgs")]
pub struct ContainerRepositoryDetailsQuery {
    #[arguments(id: $id)]
    pub container_repository: Option<ContainerRepositoryTags>
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab", graphql_type = "ContainerRepositoryConnection")]
pub struct ContainerRepositoryConnection {
    pub nodes: Option<Vec<Option<ContainerRepository>>>,
    pub page_info: PageInfo
}

/// A lighter query to just get a project's container repositories
#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab", graphql_type = "Project")]
pub struct ProjectContainerRepositoriesFragment {
    pub id: IdString,
    pub container_repositories: Option<ContainerRepositoryConnection>
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab", graphql_type = "Query", variables = "SingleProjectQueryArguments")]
pub struct ProjectContainerRepositoriesQuery {
    #[arguments(fullPath: $full_path)]
    pub project: Option<ProjectContainerRepositoriesFragment>
}
