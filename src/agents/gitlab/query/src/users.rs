#![allow(non_camel_case_types)]

use crate::projects::ProjectMemberConnection;
use crate::PageInfo;
use cynic::Id;
use gitlab_schema::gitlab::{self as schema};
use gitlab_schema::{DateString, DateTimeString};
use rkyv::{Archive, Deserialize, Serialize};
use std::fmt;

/// -----------------  CAUTION!!!!!! DO NOT CHANGE THESE VARIANTS UNLESS THE UNDERLYING SCHEMA HAS CHANGED  -----------------
///
/// Typically, Rust enums should be named in UpperCamelCase, but the GitLab GraphQL schema
/// breaks convention by not using SCREAMING_SNAKE_CASE for this enum. Cynic tries to match
/// Rust variants to their SCREAMING_SNAKE_CASE GraphQL equivalents when provided with typical
/// PascalCase enums, but it will fail unless we match the schema directly.
/// Reference: https://cynic-rs.dev/derives/enums
#[derive(cynic::Enum, Clone, Copy, Deserialize, Serialize, Archive, Debug)]
#[cynic(schema = "gitlab", rename_all = "None")]
pub enum UserState {
    active,
    banned,
    blocked,
    blocked_pending_approval,
    deactivated,
    ldap_blocked,
    #[cynic(fallback)]
    unknown,
}

impl fmt::Display for UserState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state_str = match self {
            UserState::active => "active",
            UserState::banned => "banned",
            UserState::blocked => "blocked",
            UserState::blocked_pending_approval => "blocked_pending_approval",
            UserState::deactivated => "deactivated",
            UserState::ldap_blocked => "ldap_blocked",
            UserState::unknown => "unknown",
        };
        write!(f, "{}", state_str)
    }
}

#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab")]
pub struct UserCoreConnection {
    pub count: i32,
    pub nodes: Option<Vec<Option<UserCoreFragment>>>,
}

#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab", graphql_type = "UserCoreConnection")]
pub struct UserCoreGroupsConnection {
    pub count: i32,
    pub edges: Option<Vec<Option<UserCoreEdge>>>,
    pub nodes: Option<Vec<Option<UserCoreGroups>>>,
    pub page_info: PageInfo,
}

/// GitLab's core user representation. Add fields here to get more data back.
#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab", graphql_type = "UserCore")]
pub struct UserCoreFragment {
    pub id: gitlab_schema::IdString,
    pub bot: bool,
    pub username: Option<String>,
    pub name: String,
    pub state: UserState,
    pub organization: Option<String>,
    pub web_url: String,
    pub web_path: String,
    pub last_activity_on: Option<DateString>,
    pub location: Option<String>,
    pub created_at: Option<DateTimeString>,
    pub project_memberships: Option<ProjectMemberConnection>,
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab", graphql_type = "UserCore")]
pub struct UserCoreGroups {
    pub id: gitlab_schema::IdString,
    pub groups: Option<crate::groups::GroupConnection>,
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab", graphql_type = "UserCore")]
pub struct UserCoreProjects {
    pub id: gitlab_schema::IdString,
    pub project_memberships: Option<ProjectMemberConnection>,
}

#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab")]
pub struct UserCoreEdge {
    // These fields are required by the GraphQL query but not directly used in Rust code.
    #[allow(dead_code)]
    cursor: String,
    #[allow(dead_code)]
    node: Option<UserCoreFragment>,
}

/// Arguments type for the User Observer. Akin to the query.users parameters in the schema
///
/// This datatype represents the arguments that will populate the query to the GraphQL database.
#[derive(cynic::QueryVariables, Debug, Clone)]
pub struct MultiUserQueryArguments {
    pub after: Option<String>,
    pub admins: Option<bool>,
    pub active: Option<bool>,
    pub ids: Option<Vec<Id>>,
    pub usernames: Option<Vec<String>>,
    pub humans: Option<bool>,
}

#[derive(cynic::QueryFragment)]
#[cynic(
    schema = "gitlab",
    graphql_type = "Query",
    variables = "MultiUserQueryArguments"
)]
pub struct MultiUserQuery {
    #[arguments(ids: $ids, usernames: $usernames, admins: $admins)]
    pub users: Option<UserCoreConnection>,
}
