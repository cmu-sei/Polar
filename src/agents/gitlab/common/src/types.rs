/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

use gitlab_queries::{
    groups::GroupData,
    groups::GroupMemberConnection,
    runners::{CiRunner, CiRunnerConnection},
    Project, ProjectConnection, ProjectMemberConnection, UserCoreFragment,
};
use gitlab_schema::IdString;

use rkyv::{Archive, Deserialize, Serialize};

/// This enum mostly serves as a way to inform the deserializer what datatype to map the bytes into.
/// The underlying byte vector contains a message meant for some consumer on a given topic
#[derive(Serialize, Deserialize, Archive)]
pub enum GitlabData {
    Users(Vec<UserCoreFragment>),
    Projects(Vec<Project>),
    ProjectMembers(ResourceLink<ProjectMemberConnection>),
    ProjectRunners(ResourceLink<CiRunnerConnection>),
    Groups(Vec<GroupData>),
    GroupMembers(ResourceLink<GroupMemberConnection>),
    GroupRunners(ResourceLink<CiRunnerConnection>),
    GroupProjects(ResourceLink<ProjectConnection>),
    Runners(Vec<CiRunner>),
    // RunnerJob((u32, Job)),
    // Jobs(Vec<Job>),
    // Pipelines(Vec<Pipeline>),
    // PipelineJobs(ResourceLink<Job>)
}

/// Helper type to link connection types to a resource's id
/// For example, a user or group to projects, or a group to users, etc.
#[derive(Serialize, Deserialize, Archive)]
pub struct ResourceLink<T> {
    pub resource_id: IdString,
    pub connection: T,
}

// #[derive (Serialize, Deserialize, Archive, Clone, PartialEq, Debug)]
// pub struct Runner {
//     pub id: u32,
//     pub paused: Option<bool>,
//     pub is_shared: Option<bool>,

//     pub description: Option<String>,
//     pub ip_address: Option<String>,
//     pub runner_type: String,

//     pub name: Option<String>,
//     #[serde(default)]
//     pub online: Option<bool>,
//     pub status: String,
// }

// #[derive (Serialize, Deserialize, Archive, Clone, PartialEq, Debug)]
// pub struct Pipeline {
//     pub id: u32,
//     pub project_id: Option<u32>,
//     pub status: String,
//     pub source: String,
//     pub sha: String,
//     pub created_at: String,
//     pub updated_at: String
// }
// #[derive (Serialize, Deserialize, Archive, Clone, PartialEq, Debug)]

// pub struct Job {
//     pub id: u32,

//     pub ip_address: Option<String>,
//     pub status: String,
//     pub stage: String,
//     pub name: String,
//     //TODO: implement custom serailization and deserialization for this struct,
//     //      consumer can't read "ref" field, observer can't use it.
//     // #[serde(rename(deserialize = "ref", serialize = "git_ref"))]
//     // pub git_ref: String,
//     pub created_at: String,
//     pub started_at: Option<String>,
//     pub finished_at: Option<String>,
//     pub erased_at: Option<String>,
//     pub duration: Option<f64>,
//     pub user: Option<User>,
//     pub commit: GitCommit,
//     pub pipeline: Pipeline,
//     pub project: Value,
//     // TODO: Project data read from the pipeline jobs api only returns one field, vs the runner jobs api which returns a full project
//     //implement custom deserialziation to handle this case
//     pub allow_failure: bool,
//     pub runner: Option<Runner>,
//     pub failure_reason: Option<String>,
//     pub web_url: String,
//     pub coverage: Option<Value>
// }
// #[derive (Serialize, Deserialize, Clone, PartialEq, Debug)]
// pub struct GitCommit {
//     pub id: String,
//     pub short_id: String,
//     pub title: String,
//     pub created_at: String,
//     #[serde(default)]
//     pub parent_ids: Option<Vec<String>>,
//     pub message: String,
//     pub author_name: String,
//     pub authored_date: String,
//     pub committed_date: String
// }
// #[derive (Serialize, Deserialize, Clone, PartialEq, Debug)]
// pub struct ContainerRegistry {
//     pub id: u32,
//     pub name: String,
//     pub path: String,
//     pub project_id: u32,
//     pub created_at: String,
//     pub cleanup_policy_started_at: String,
//     pub tags: Value
// }
