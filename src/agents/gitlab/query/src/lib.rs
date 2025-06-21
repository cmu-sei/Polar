use gitlab_schema::gitlab::{self as schema};
use gitlab_schema::IdString;
use projects::AccessLevelEnum;
use rkyv::Deserialize;
use rkyv::Serialize;

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
