use std::{env, error::Error};
use ractor::Actor;
use common::init_logging;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error> > {
    init_logging();
    
    // info!("Scheduling observers");
    // TODO: Pass in values from config through args  
    let client_cert_file = env::var("TLS_CLIENT_CERT").unwrap();
    let client_private_key_file = env::var("TLS_CLIENT_KEY").unwrap();
    let ca_cert_file =  env::var("TLS_CA_CERT").unwrap();   
    
    let broker_addr = env::var("BROKER_ADDR").unwrap();

    // let args = todo_observer::actors::ObserverSupervisorArgs {
        // broker_addr,
        // client_cert_file,
        // client_private_key_file,
        // ca_cert_file: ca_cert_file,
    // };

    // let (supervisor, handle) = Actor::spawn(Some("TODO_APP_OBSERVER".to_string()), todo_observer::actors::ObserverSupervisor,args).await.expect("Expected to start observer agent");
    // let _ = handle.await;
    
    Ok(())
}
