use crate::projects::ProjectCoreConnection;
use crate::users::UserCoreFragment;
use gitlab_schema::gitlab::{self as schema};
use gitlab_schema::DateTimeString;
use gitlab_schema::IdString;

use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;

#[derive(cynic::QueryVariables, Debug, Clone, Deserialize, Serialize, Archive)]
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

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab")]
pub struct GroupConnection {
    pub edges: Option<Vec<Option<GroupEdge>>>,
    pub nodes: Option<Vec<Option<GroupData>>>,
    pub page_info: crate::PageInfo,
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab")]
pub struct GroupEdge {
    pub cursor: String,
    pub node: Option<GroupData>,
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab", graphql_type = "Group")]
pub struct GroupData {
    pub id: IdString,
    pub full_name: String,
    pub full_path: IdString,
    pub description: Option<String>,
    pub created_at: Option<DateTimeString>,
    pub marked_for_deletion_on: Option<DateTimeString>,
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab", graphql_type = "Group")]
pub struct GroupMembersFragment {
    pub id: IdString,
    pub group_members_count: i32,
    pub group_members: Option<GroupMemberConnection>,
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab", graphql_type = "Group")]
pub struct GroupProjectsFragment {
    pub id: IdString,
    pub projects: Option<ProjectCoreConnection>,
    pub projects_count: i32,
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab", graphql_type = "Group")]
pub struct GroupRunnersFragment {
    pub id: IdString,
    pub runners: Option<crate::runners::CiRunnerIdConnection>,
}

/// Datatype representing a users's membership for a group
#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab")]
pub struct GroupMember {
    /// GitLab::Access level.
    pub access_level: Option<crate::AccessLevel>,

    /// Date and time the membership was created.
    pub created_at: Option<DateTimeString>,

    // User that authorized membership.
    // TODO: This would be useful to know
    // pub created_by: Option<UserCore>,
    /// Date and time the membership expires.
    pub expires_at: Option<DateTimeString>,

    /// Group that a user is a member of.
    // pub group: Option<Group>,

    // ID of the member.
    pub id: IdString,

    /// Group notification email for user. Only available for admins.
    pub notification_email: Option<String>,

    /// Date and time the membership was last updated.
    pub updated_at: Option<DateTimeString>,

    // User that is associated with the member object.
    pub user: Option<UserCoreFragment>,
    // Permissions for the current user on the resource.
    // pub user_permissions: GroupPermissions,
}

#[derive(cynic::QueryFragment, Deserialize, Clone, Serialize, Archive)]
#[cynic(schema = "gitlab")]
pub struct GroupMemberConnection {
    // pub edges: Option<Vec<Option<GroupMemberEdge>>>,
    pub nodes: Option<Vec<Option<GroupMember>>>,
    pub page_info: crate::PageInfo,
}

#[derive(cynic::QueryFragment, Clone, Deserialize, Serialize, Archive)]
#[cynic(schema = "gitlab")]
pub struct GroupMemberEdge {
    pub cursor: String,
    pub node: Option<GroupMember>,
}

// #[derive(cynic::QueryFragment, Debug, Clone)]
// #[cynic(schema = "gitlab")]
// pub struct GroupPermissions {

// }

#[derive(cynic::QueryVariables)]
pub struct GroupPathVariable {
    pub full_path: IdString,
}

#[derive(cynic::QueryFragment)]
#[cynic(
    schema = "gitlab",
    graphql_type = "Query",
    variables = "MultiGroupQueryArguments"
)]
pub struct AllGroupsQuery {
    #[arguments(sort: $sort)]
    pub groups: Option<GroupConnection>,
}

#[derive(cynic::QueryFragment)]
#[cynic(
    schema = "gitlab",
    graphql_type = "Query",
    variables = "GroupPathVariable"
)]
pub struct GroupMembersQuery {
    #[arguments(fullPath: $full_path)]
    pub group: Option<GroupMembersFragment>,
}

#[derive(cynic::QueryFragment)]
#[cynic(
    schema = "gitlab",
    graphql_type = "Query",
    variables = "GroupPathVariable"
)]
pub struct GroupProjectsQuery {
    #[arguments(fullPath: $full_path)]
    pub group: Option<GroupProjectsFragment>,
}

#[derive(cynic::QueryFragment)]
#[cynic(
    schema = "gitlab",
    graphql_type = "Query",
    variables = "GroupPathVariable"
)]
pub struct GroupRunnersQuery {
    #[arguments(fullPath: $full_path)]
    pub group: Option<GroupRunnersFragment>,
}
