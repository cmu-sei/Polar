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
    groups::{GroupData, GroupMemberConnection},
    projects::{
        ContainerRepository, ContainerRepositoryTag, GitlabCiJob, Package, Pipeline, Project,
        ProjectConnection, ProjectMemberConnection,
    },
    runners::{CiRunner, CiRunnerConnection},
    users::UserCoreFragment,
    LicenseHistoryEntry, Metadata,
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
    Jobs((String, Vec<GitlabCiJob>)),
    Pipelines((String, Vec<Pipeline>)),
    ProjectPackages((String, Vec<Package>)),
    ProjectContainerRepositories((String, Vec<ContainerRepository>)),
    ContainerRepositoryTags((String, Vec<ContainerRepositoryTag>)),
    Instance(GitlabInstance),
    Licenses(Vec<LicenseHistoryEntry>),
}

/// Helper type to link connection types to a resource's id
/// For example, a user or group to projects, or a group to users, etc.
#[derive(Serialize, Deserialize, Archive)]
pub struct ResourceLink<T> {
    pub resource_id: IdString,
    pub connection: T,
}

#[derive(Serialize, Deserialize, Archive)]
pub struct GitlabInstance {
    /// TODO: add a unique instance id
    // pub instance_id: String
    // TODO: Add last seen at timestamp
    // pub last_seen_at: chrono::DateTime?
    pub base_url: String,
    pub metadata: Metadata,
}

#[derive(Debug, Deserialize, Serialize, Archive)]
pub struct GitlabVersion {
    pub version: String,
    pub revision: String,
}

#[derive(Debug, Deserialize, Serialize, Archive)]
pub struct GitlabMetadata {
    pub enterprise: bool,
}

#[derive(Debug, Deserialize, Serialize, Archive)]
pub struct GitlabLicense {
    pub id: String,
    pub plan: String,
    pub starts_at: String,
    pub expires_at: String,
    pub active_users: u32,
}
