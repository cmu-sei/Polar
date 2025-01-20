use gitlab_schema::gitlab::{self as schema, UserID};
use cynic::*;
use serde::Serialize;

/// NOTE: Cynic matches rust variants up to their equivalent SCREAMING_SNAKE_CASE GraphQL variants. 
/// This behaviour is disabled because the gitlab schema goes against this
/// TODO: Disable warnings on this datatype
#[derive(cynic::Enum, Clone, Copy, Debug)]
#[cynic(schema = "gitlab", rename_all = "None")]
pub enum UserState {
    active, 
    banned, 
    blocked,
    blocked_pending_approval,
    deactivated,
    ldap_blocked
}



// pub enum UserState {
//     Active, 
//     Banned, 
//     Blocked,
//     BlockedPendingApproval,
//     Deactivated,
//     LdapBlocked
// }


#[derive(cynic::QueryVariables)]
pub struct UserCoreQueryArguments {
    id: Option<Id>,
    username: Option<String>
}


#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab")]
pub struct UserCore {
    id: Id,
    bot: bool,
    username: Option<String>,
    name: String,
    // status: Option<schema::UserStatus>,
    // state: schema::UserState,
    // last_activity_on: Option<schema::Date>,
    // location: Option<String>,
    // created_at: Option<schema::Time>
}

#[derive(cynic::QueryFragment)]
#[cynic(schema = "gitlab", graphql_type = "Query", variables = "UserCoreQueryArguments")]
pub struct UserQuery {
    #[arguments(id: $id, username: $username)]
    user: Option<UserCore>
}