use crate::projects::ProjectMemberConnection;
use crate::PageInfo;
use cynic::*;
use gitlab_schema::gitlab::{self as schema};
use gitlab_schema::DateString;
use gitlab_schema::DateTimeString;
use rkyv::Deserialize;
use rkyv::Serialize;
use std::fmt;

/// NOTE: Cynic matches rust variants up to their equivalent SCREAMING_SNAKE_CASE GraphQL variants.
/// This behaviour is disabled because the gitlab schema goes against this
/// TODO: Disable warnings on this datatype
#[derive(cynic::Enum, Clone, Copy, Deserialize, Serialize, rkyv::Archive, Debug)]
#[cynic(schema = "gitlab", rename_all = "None")]
pub enum UserState {
    active,
    banned,
    blocked,
    blocked_pending_approval,
    deactivated,
    ldap_blocked,
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
        };
        write!(f, "{}", state_str)
    }
}

#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab")]
pub struct UserCoreConnection {
    pub count: i32,
    // pub edges: Option<Vec<Option<UserCoreEdge>>>,
    pub nodes: Option<Vec<Option<UserCoreFragment>>>,
    // pub page_info: PageInfo,
}

#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab", graphql_type = "UserCoreConnection")]
pub struct UserCoreGroupsConnection {
    pub count: i32,
    pub edges: Option<Vec<Option<UserCoreEdge>>>,
    pub nodes: Option<Vec<Option<UserCoreGroups>>>,
    pub page_info: PageInfo,
}

/// Gitlab's core user representation. Add fields here to get more data back.
#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, rkyv::Archive)]
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

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab", graphql_type = "UserCore")]
pub struct UserCoreGroups {
    pub id: gitlab_schema::IdString,
    pub groups: Option<crate::groups::GroupConnection>,
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab", graphql_type = "UserCore")]
pub struct UserCoreProjects {
    pub id: gitlab_schema::IdString,
    pub project_memberships: Option<ProjectMemberConnection>,
}

#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab")]
pub struct UserCoreEdge {
    cursor: String,
    node: Option<UserCoreFragment>,
}

/// Arguments type for the User Observer. Akin to the query.users parameters in the schema
///
/// This datatype represents the arguments that will populate the query to the graphql database.
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
