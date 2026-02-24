use git_repo_observer::{supervisor::RootSupervisor, SERVICE_NAME};
use ractor::Actor;

#[tokio::main]
async fn main() {
    polar::init_logging(SERVICE_NAME.to_string());

    //introspect invironment to generate args
    let (_agent, handle) = Actor::spawn(
        Some(format!("{SERVICE_NAME}.supervisor")),
        RootSupervisor,
        (),
    )
    .await
    .expect("Expected agent to start");
    handle.await.unwrap();
}
