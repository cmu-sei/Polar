use gitlab_schema::gitlab::{self as schema};
use gitlab_schema::{DateString, IdString};
use projects::AccessLevelEnum;
use rkyv::Serialize;
use rkyv::{Archive, Deserialize};

pub mod groups;
pub mod projects;
pub mod runners;
pub mod users;

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

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
pub struct Metadata {
    pub enterprise: bool,
    pub kas: Kas,
    pub revision: String,
    pub version: String,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
pub struct Kas {
    pub enabled: bool,
    // pub external_k8s_proxy_url: Option<String>,
    pub external_url: Option<String>,
    pub version: Option<String>,
}

#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive, Clone)]
#[cynic(graphql_type = "Query")]
pub struct MetadataQuery {
    pub metadata: Option<Metadata>,
}

/// A single entry from the GitLab cloud license history.
#[derive(Debug, Clone, cynic::QueryFragment, Serialize, Deserialize, Archive)]
#[cynic(graphql_type = "LicenseHistoryEntry")]
pub struct LicenseHistoryEntry {
    /// ID of the license entry.
    pub id: IdString,
    /// Date when the license was added.
    pub created_at: Option<DateString>,
    /// Date when the license started.
    pub starts_at: Option<DateString>,
    /// Date when the license expires.
    pub expires_at: Option<DateString>,
    /// Date when license-locked features are blocked.
    pub block_changes_at: Option<DateString>,
    /// Date when the license was activated.
    pub activated_at: Option<DateString>,

    /// Name of the licensee.
    pub name: Option<String>,
    /// Email of the licensee.
    pub email: Option<String>,
    /// Company of the licensee.
    pub company: Option<String>,

    /// Name of the subscription plan.
    pub plan: String,
    /// Type of the license (e.g., `cloud`, `on-premise`, etc).
    /// type is a reserved word, so this will suffice
    #[cynic(rename = "type")]
    pub entry_type: String,
    /// Number of paid users included in the license.
    pub users_in_license_count: Option<i32>,
}

/// A connection object for paginated access to license history entries.
#[derive(Debug, Clone, cynic::QueryFragment)]
pub struct LicenseHistoryEntryConnection {
    /// All entries returned in this page.
    pub nodes: Option<Vec<Option<LicenseHistoryEntry>>>,
    // Optional: add `pageInfo` here if you want to implement pagination.
}

/// A query to retreive license history
#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query")]
pub struct LicenseHistoryQuery {
    pub license_history_entries: Option<LicenseHistoryEntryConnection>,
}
