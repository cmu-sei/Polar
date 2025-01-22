use coercions::CoercesTo;
use gitlab_schema::gitlab::{self as schema};
use cynic::*;
use serde::Serialize;

// #[derive(cynic::Scalar, serde::Serialize,serde::Deserialize, Clone, Debug)]
// #[cynic(schema = "gitlab", graphql_type = "UserID")]
// pub struct UserID(Id);


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

#[derive(cynic::QueryVariables, serde::Deserialize, Debug)]
pub struct MultiUserQueryArguments {
    pub admins: Option<bool>,
    pub active: Option<bool>,
    pub ids: Option<Vec<Id>>,
    pub usernames: Option<Vec<String>>,
    pub humans: Option<bool>

}

#[derive(cynic::QueryFragment, serde::Serialize, Debug)]
#[cynic(schema = "gitlab")]
pub struct UserCoreConnection {
    pub count: i32, 	  //Int! 	Total count of collection.
    pub edges: Option<Vec<Option<UserCoreEdge>>>,	  
    pub nodes: Option<Vec<Option<UserCore>>>,	  //[UserCore] 	A list of nodes.
    pub pageInfo: PageInfo, // 	PageInfo! 	Information to aid in pagination. 
}

#[derive(cynic::QueryFragment, serde::Serialize, Debug)]
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

#[derive(cynic::QueryFragment, serde::Serialize, Debug)]
#[cynic(schema = "gitlab")]
pub struct UserCore {
    id: Id,
    bot: bool,
    username: Option<String>,
    name: String,
    // status: Option<schema::UserStatus>,
    state: UserState,
    // last_activity_on: Option<schema::Date>,
    location: Option<String>,
    // created_at: Option<schema::Time>
}

#[derive(cynic::QueryFragment, serde::Serialize, Debug)]
#[cynic(schema = "gitlab")]
pub struct UserCoreEdge {
    cursor: String,
    node: Option<UserCore>
}


#[derive(cynic::QueryFragment, Debug, serde::Serialize)]
#[cynic(schema = "gitlab", graphql_type = "Query", variables = "MultiUserQueryArguments")]
pub struct MultiUserQuery {
    #[arguments(ids: $ids, usernames: $usernames)]
    pub users: Option<UserCoreConnection>
}


// #[derive(cynic::QueryFragment)]
// #[cynic(schema = "gitlab", graphql_type = "Query", variables = "UserCoreQueryArguments")]
// pub struct UserQuery {
//     #[arguments(id: $id, username: $username)]
//     user: Option<UserCore>
// }

