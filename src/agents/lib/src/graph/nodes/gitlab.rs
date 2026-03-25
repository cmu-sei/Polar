use crate::graph::controller::GraphNodeKey;
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
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            GitlabNodeKey::GitlabInstance { instance_id } => (
                format!("({prefix}:GitlabInstance {{ id: ${prefix}_instance_id }})"),
                vec![(format!("{prefix}_instance_id"), instance_id.clone().into())],
            ),
            GitlabNodeKey::License {
                instance_id,
                license_id,
            } => (
                format!(
                    "({prefix}:GitlabLicense {{ instance_id: ${prefix}_instance_id, license_id: ${prefix}_license_id  }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_license_id"), license_id.clone().into()),
                ],
            ),
            GitlabNodeKey::Project {
                instance_id,
                project_id,
            } => (
                format!(
                    "({prefix}:GitlabProject {{ instance_id: ${prefix}_instance_id, project_id: ${prefix}_project_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (
                        format!("{prefix}_project_id"),
                        (project_id).to_owned().into(),
                    ),
                ],
            ),

            GitlabNodeKey::User {
                instance_id,
                user_id,
            } => (
                format!(
                    "({prefix}:GitlabUser {{ instance_id: ${prefix}_instance_id, user_id: ${prefix}_user_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_user_id"), (user_id).to_owned().into()),
                ],
            ),
            GitlabNodeKey::Group {
                instance_id,
                group_id,
            } => (
                format!(
                    "({prefix}:GitlabGroup {{ instance_id: ${prefix}_instance_id, group_id: ${prefix}_group_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_group_id"), (group_id).to_owned().into()),
                ],
            ),
            GitlabNodeKey::ContainerRepository {
                repository_id,
                project_id,
            } => (
                format!(
                    "({prefix}:GitlabContainerRepository {{ project_id: ${prefix}_project_id, repository_id: ${prefix}_repository_id }})"
                ),
                vec![
                    (
                        format!("{prefix}_repository_id"),
                        (repository_id).to_owned().into(),
                    ),
                    (
                        format!("{prefix}_project_id"),
                        (project_id).to_owned().into(),
                    ),
                ],
            ),
            GitlabNodeKey::ContainerRepositoryTag {
                repository_id,
                digest,
            } => (
                format!(
                    "({prefix}:ContainerRepositoryTag {{ repository_id: ${prefix}_repository_id, digest: ${prefix}_digest }})"
                ),
                vec![
                    (
                        format!("{prefix}_repository_id"),
                        (repository_id).to_owned().into(),
                    ),
                    (format!("{prefix}_digest"), (digest).to_owned().into()),
                ],
            ),
            GitlabNodeKey::Pipeline {
                instance_id,
                pipeline_id,
            } => (
                format!(
                    "({prefix}:GitlabPipeline {{  instance_id: ${prefix}_instance_id, pipeline_id: ${prefix}_pipeline_id }})"
                ),
                vec![
                    (
                        format!("{prefix}_instance_id"),
                        instance_id.to_owned().into(),
                    ),
                    (
                        format!("{prefix}_pipeline_id"),
                        pipeline_id.to_owned().into(),
                    ),
                ],
            ),

            GitlabNodeKey::Job {
                instance_id,
                job_id,
            } => (
                format!(
                    "({prefix}:GitlabJob {{ instance_id: ${prefix}_instance_id, job_id: ${prefix}_job_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_job_id"), job_id.to_owned().into()),
                ],
            ),

            GitlabNodeKey::Runner {
                instance_id,
                runner_id,
            } => (
                format!(
                    "({prefix}:GitlabRunner {{ instance_id: ${prefix}_instance_id, runner_id: ${prefix}_runner_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_runner_id"), runner_id.to_owned().into()),
                ],
            ),

            GitlabNodeKey::PipelineArtifact {
                instance_id,
                artifact_id,
            } => (
                format!(
                    "({prefix}:GitlabPipelineArtifact {{ instance_id: ${prefix}_instance_id, name: ${prefix}_artifact_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_artifact_id"), artifact_id.clone().into()),
                ],
            ),
            GitlabNodeKey::Package { package_id } => (
                format!("({prefix}:GitlabPackage {{ package_id: ${prefix}_package_id }})"),
                vec![(format!("{prefix}_package_id"), package_id.clone().into())],
            ),
            GitlabNodeKey::PackageFile {
                file_id,
                package_id,
            } => (
                format!(
                    "({prefix}:GitlabPackageFile {{ file_id: ${prefix}_file_id, repository_id: ${prefix}_package_id}})"
                ),
                vec![
                    (format!("{prefix}_package_id"), package_id.clone().into()),
                    (format!("{prefix}_file_id"), file_id.to_owned().into()),
                ],
            ),
        }
    }
}
