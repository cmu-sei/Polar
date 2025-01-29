use serde::{Deserialize, Serialize};
// use gitlab_queries::UserCore;

// pub mod serialize;

// // #[derive (Serialize, Deserialize, Debug)]
// pub enum MessageType {
//     GitlabMessage(GitlabData),
//     ConfigMessage
// }

// pub enum GitlabData {
//     Users(Vec<UserCore>),
//     // Projects(Vec<Project>),
//     // Groups(Vec<UserGroup>),
//     // ProjectUsers(ResourceLink<User>),
//     // ProjectRunners(ResourceLink<Runner>),
//     // GroupMembers(ResourceLink<User>),
//     // GroupRunners(ResourceLink<Runner>),
//     // GroupProjects(ResourceLink<Project>),
//     // Runners(Vec<Runner>),
//     // RunnerJob((u32, Job)),
//     // Jobs(Vec<Job>),
//     // Pipelines(Vec<Pipeline>),
//     // PipelineJobs(ResourceLink<Job>)
// }

pub fn init_logging() {
    let dir = tracing_subscriber::filter::Directive::from(tracing::Level::DEBUG);

    use std::io::stderr;
    use std::io::IsTerminal;
    use tracing_glog::Glog;
    use tracing_glog::GlogFields;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    let fmt = tracing_subscriber::fmt::Layer::default()
        .with_ansi(stderr().is_terminal())
        .with_writer(std::io::stderr)
        .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
        .fmt_fields(GlogFields::default().compact());

    let filter = vec![dir]
        .into_iter()
        .fold(EnvFilter::from_default_env(), |filter, directive| {
            filter.add_directive(directive)
        });

    let subscriber = Registry::default().with(filter).with(fmt);
    tracing::subscriber::set_global_default(subscriber).expect("to set global subscriber");
}


