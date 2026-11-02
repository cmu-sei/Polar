/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

use crate::graph::controller::{GraphNodeKey, NodeIdentity, NodeKeyVocabulary};
use neo4rs::BoltType;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GitlabNodeKey {
    /// A GitLab instance (SaaS or self-managed)
    GitlabInstance {
        instance_id: String, // UUIDv5 or canonical hostname hash
    },
    License {
        instance_id: String,
        license_id: String,
    },
    /// A project/repository
    Project {
        instance_id: String,
        project_id: String, // GitLab internal project ID
    },

    /// A GitLab user
    User {
        instance_id: String,
        user_id: String,
    },

    Group {
        instance_id: String,
        group_id: String,
    },

    /// A container registry repository
    ContainerRepository {
        project_id: String,
        repository_id: String,
    },
    Package {
        package_id: String,
    },
    PackageFile {
        file_id: String,
        package_id: String,
    },
    ContainerRepositoryTag {
        repository_id: String,
        digest: String,
    },

    /// A CI pipeline run
    Pipeline {
        instance_id: String,
        pipeline_id: String,
    },

    /// A CI job
    Job {
        instance_id: String,
        job_id: String,
    },

    /// A GitLab runner
    Runner {
        instance_id: String,
        runner_id: String,
    },

    /// A pipeline artifact (logical artifact, not individual files)
    PipelineArtifact {
        instance_id: String,
        artifact_id: String,
    },
}

impl GraphNodeKey for GitlabNodeKey {
    fn identity(&self) -> NodeIdentity {
        let s = |v: &String| BoltType::String(v.clone().into());
        match self {
            GitlabNodeKey::GitlabInstance { instance_id } => {
                NodeIdentity::fixed(&["GitlabInstance"], vec![("id", s(instance_id))])
            }
            GitlabNodeKey::License {
                instance_id,
                license_id,
            } => NodeIdentity::fixed(
                &["GitlabLicense"],
                vec![
                    ("instance_id", s(instance_id)),
                    ("license_id", s(license_id)),
                ],
            ),
            GitlabNodeKey::Project {
                instance_id,
                project_id,
            } => NodeIdentity::fixed(
                &["GitlabProject"],
                vec![
                    ("instance_id", s(instance_id)),
                    ("project_id", s(project_id)),
                ],
            ),
            GitlabNodeKey::User {
                instance_id,
                user_id,
            } => NodeIdentity::fixed(
                &["GitlabUser"],
                vec![("instance_id", s(instance_id)), ("user_id", s(user_id))],
            ),
            GitlabNodeKey::Group {
                instance_id,
                group_id,
            } => NodeIdentity::fixed(
                &["GitlabGroup"],
                vec![("instance_id", s(instance_id)), ("group_id", s(group_id))],
            ),
            GitlabNodeKey::ContainerRepository {
                repository_id,
                project_id,
            } => NodeIdentity::fixed(
                &["GitlabContainerRepository"],
                vec![
                    ("project_id", s(project_id)),
                    ("repository_id", s(repository_id)),
                ],
            ),
            GitlabNodeKey::ContainerRepositoryTag {
                repository_id,
                digest,
            } => NodeIdentity::fixed(
                &["ContainerRepositoryTag"],
                vec![("repository_id", s(repository_id)), ("digest", s(digest))],
            ),
            GitlabNodeKey::Pipeline {
                instance_id,
                pipeline_id,
            } => NodeIdentity::fixed(
                &["GitlabPipeline"],
                vec![
                    ("instance_id", s(instance_id)),
                    ("pipeline_id", s(pipeline_id)),
                ],
            ),
            GitlabNodeKey::Job {
                instance_id,
                job_id,
            } => NodeIdentity::fixed(
                &["GitlabJob"],
                vec![("instance_id", s(instance_id)), ("job_id", s(job_id))],
            ),
            GitlabNodeKey::Runner {
                instance_id,
                runner_id,
            } => NodeIdentity::fixed(
                &["GitlabRunner"],
                vec![("instance_id", s(instance_id)), ("runner_id", s(runner_id))],
            ),
            GitlabNodeKey::PipelineArtifact {
                instance_id,
                artifact_id,
            } => NodeIdentity::fixed(
                &["GitlabPipelineArtifact"],
                vec![("instance_id", s(instance_id)), ("name", s(artifact_id))],
            ),
            GitlabNodeKey::Package { package_id } => {
                NodeIdentity::fixed(&["GitlabPackage"], vec![("package_id", s(package_id))])
            }
            // The package's id is written as `repository_id`, as it always was.
            GitlabNodeKey::PackageFile {
                file_id,
                package_id,
            } => NodeIdentity::fixed(
                &["GitlabPackageFile"],
                vec![("file_id", s(file_id)), ("repository_id", s(package_id))],
            ),
        }
    }
}

impl NodeKeyVocabulary for GitlabNodeKey {
    fn every_variant() -> Vec<Self> {
        let s = || "x".to_string();
        let all = vec![
            GitlabNodeKey::GitlabInstance { instance_id: s() },
            GitlabNodeKey::License {
                instance_id: s(),
                license_id: s(),
            },
            GitlabNodeKey::Project {
                instance_id: s(),
                project_id: s(),
            },
            GitlabNodeKey::User {
                instance_id: s(),
                user_id: s(),
            },
            GitlabNodeKey::Group {
                instance_id: s(),
                group_id: s(),
            },
            GitlabNodeKey::ContainerRepository {
                project_id: s(),
                repository_id: s(),
            },
            GitlabNodeKey::Package { package_id: s() },
            GitlabNodeKey::PackageFile {
                file_id: s(),
                package_id: s(),
            },
            GitlabNodeKey::ContainerRepositoryTag {
                repository_id: s(),
                digest: s(),
            },
            GitlabNodeKey::Pipeline {
                instance_id: s(),
                pipeline_id: s(),
            },
            GitlabNodeKey::Job {
                instance_id: s(),
                job_id: s(),
            },
            GitlabNodeKey::Runner {
                instance_id: s(),
                runner_id: s(),
            },
            GitlabNodeKey::PipelineArtifact {
                instance_id: s(),
                artifact_id: s(),
            },
        ];
        // A variant added to the enum does not compile until it is named here, beside the list it must join.
        for key in &all {
            match key {
                GitlabNodeKey::GitlabInstance { .. }
                | GitlabNodeKey::License { .. }
                | GitlabNodeKey::Project { .. }
                | GitlabNodeKey::User { .. }
                | GitlabNodeKey::Group { .. }
                | GitlabNodeKey::ContainerRepository { .. }
                | GitlabNodeKey::Package { .. }
                | GitlabNodeKey::PackageFile { .. }
                | GitlabNodeKey::ContainerRepositoryTag { .. }
                | GitlabNodeKey::Pipeline { .. }
                | GitlabNodeKey::Job { .. }
                | GitlabNodeKey::Runner { .. }
                | GitlabNodeKey::PipelineArtifact { .. } => {}
            }
        }
        all
    }
}
