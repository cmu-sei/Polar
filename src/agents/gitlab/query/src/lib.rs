use std::fmt;

use gitlab_schema::gitlab::{self as schema};
use cynic::*;
use gitlab_schema::DateTimeString;
use gitlab_schema::IdString;
use rkyv::Archive;
use rkyv::Serialize;
use rkyv::Deserialize;


// #[derive(cynic::Scalar,serde::Deserialize, Clone, Debug)]
// #[cynic(schema = "gitlab", graphql_type = "UserID")]
// pub struct UserID(Id);


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
    ldap_blocked
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
#[derive(cynic::QueryFragment, Debug, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct Namespace {
    pub id: IdString,
    // pub parent_id: Option<u32>,
    pub full_name: String,
    pub full_path: IdString,

}

#[derive(cynic::QueryVariables, Debug, Clone, Deserialize, Serialize, rkyv::Archive)]
pub struct MultiGroupQueryArguments {
    //TODO: Doesn't appear to be supported in our test version of gitlab, but it is mentioned as as an argument in the docs
    // pub ids: Option<Vec<IdString>>,
    pub search: Option<String>,
    //NOTE: Gitlab has an expected format for this input
    //REFERENCE: https://docs.gitlab.com/17.7/ee/api/graphql/reference/#querygroups
    pub sort: String,
    pub marked_for_deletion_on: Option<DateTimeString>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub first: Option<i32>,
    pub last: Option<i32>,
}

#[derive(cynic::QueryFragment, Debug, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct GroupConnection {
    pub edges: Option<Vec<Option<GroupEdge>>>,
    pub nodes: Option<Vec<Option<Group>>>,
    pub page_info: PageInfo,
}

#[derive(cynic::QueryFragment, Debug, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct GroupEdge {
    pub cursor: String,
    pub node: Option<Group>,
}

#[derive(cynic::QueryFragment, Debug, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct Group {
    pub id: IdString,
    pub full_name: String,
    pub full_path: IdString,
    pub description: Option<String>,
    pub created_at: Option<DateTimeString>,
    pub marked_for_deletion_on: Option<DateTimeString>,
    pub group_members_count: i32
}

/// Datatype representing a users's membership for a group
#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab")]
pub struct GroupMember {
    /// GitLab::Access level.
    // pub access_level: Option<AccessLevel>,

    /// Date and time the membership was created.
    pub created_at: Option<DateTimeString>,

    /// User that authorized membership.
    pub created_by: Option<UserCore>,

    /// Date and time the membership expires.
    pub expires_at: Option<DateTimeString>,

    /// Group that a user is a member of.
    pub group: Option<Group>,

    /// ID of the member.
    pub id: IdString,

    /// Group notification email for user. Only available for admins.
    pub notification_email: Option<String>,

    /// Date and time the membership was last updated.
    pub updated_at: Option<DateTimeString>,

    /// User that is associated with the member object.
    pub user: Option<UserCore>,

    // Permissions for the current user on the resource.
    // pub user_permissions: GroupPermissions,
}

// #[derive(cynic::QueryFragment, Debug, Clone)]
// #[cynic(schema = "gitlab")]
// pub struct AccessLevel {
//     pub string_value: Option<String>,
//     pub integer_value: Option<i32>,
// }


// #[derive(cynic::QueryFragment, Debug, Clone)]
// #[cynic(schema = "gitlab")]
// pub struct GroupPermissions {

// }


#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab", graphql_type = "Query", variables = "MultiGroupQueryArguments")]
pub struct MultiGroupQuery {
    #[arguments(sort: $sort)]
    pub groups: Option<GroupConnection>
}


#[derive(cynic::QueryFragment, Debug, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct Project {
    pub id: IdString,
    pub name: String,
    pub full_path: IdString,
    pub description: Option<String>,
    pub created_at: Option<DateTimeString>,
    pub namespace: Option<Namespace>,
    pub last_activity_at: Option<DateTimeString>,
    pub group: Option<Group>
}

#[derive(cynic::QueryFragment, Debug, Clone,  Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab")]
pub struct ProjectEdge {
    pub cursor: String,
    pub node: Option<Project>
}

#[derive(cynic::QueryFragment, Debug, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct ProjectConnection {
    pub count: i32, 	  //Int! 	Total count of collection.
    pub edges: Option<Vec<Option<ProjectEdge>>>,	  
    pub nodes: Option<Vec<Option<Project>>>,	  //[UserCore] 	A list of nodes.
    pub page_info: PageInfo, // 	PageInfo! 	Information to aid in pagination. 
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

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(schema = "gitlab", graphql_type = "Query", variables = "MultiProjectQueryArguments")]
pub struct MultiProjectQuery {
    pub projects: Option<ProjectConnection>
}

#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab")]
pub struct UserCoreConnection {
    pub count: i32,
    // pub edges: Option<Vec<Option<UserCoreEdge>>>,	  
    pub nodes: Option<Vec<Option<UserCore>>>,
    // pub page_info: PageInfo, 
}

#[derive(cynic::QueryFragment, Debug, Clone, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct PageInfo {
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub start_cursor: Option<String>
}


// #[derive(cynic::QueryVariables, serde::Deserialize)]
// pub struct UserCoreQueryArguments {
//     id: Option<String>,
//     username: Option<String>
// }

/// Gitlab's core user representation. Add fields here to get more data back.
#[derive(cynic::QueryFragment, Deserialize, Serialize, rkyv::Archive)]
#[cynic(schema = "gitlab")]
pub struct UserCore {
    pub id: gitlab_schema::IdString,
    pub bot: bool,
    pub username: Option<String>,
    pub name: String,
    //TODO: looks like this overcomplicates the query (literally, the test instance has a complexity limit of 300, this brings it over 310)
    // pub groups: Option<GroupConnection>,
    // status: Option<schema::UserStatus>,
    pub state: UserState,
    // last_activity_on: Option<schema::Date>,
    pub location: Option<String>,
    pub created_at: Option<gitlab_schema::DateTimeString>,
    pub contributed_projects: Option<ProjectConnection>
}

#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab")]
pub struct UserCoreEdge {
    cursor: String,
    node: Option<UserCore>
}


#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab", graphql_type = "Query", variables = "MultiUserQueryArguments")]
pub struct MultiUserQuery {
    #[arguments(ids: $ids, usernames: $usernames, admins: $admins)]
    pub users: Option<UserCoreConnection>
}

