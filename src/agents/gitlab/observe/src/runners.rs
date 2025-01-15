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


use cassini::{client::TcpClientMessage, ClientMessage};
use common::{types::{GitlabData, Runner}, RUNNERS_QUEUE_NAME};

use log::{debug, error, warn};
use reqwest::Client;
use serde_json::to_string;
use crate::{get_all_elements, send, GitlabObserverArgs, GitlabObserverState};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};

use crate::BROKER_CLIENT_NAME;

const LOCK_FILE_PATH: &str = "/tmp/runners_observer.lock";


pub struct GitlabRunnerObserver;

#[async_trait]
impl Actor for GitlabRunnerObserver {
    type Msg = ();
    type State = GitlabObserverState;
    type Arguments = GitlabObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabObserverArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to instance");
        
        match Client::builder().build() {
            Ok(client) => {
                let state = GitlabObserverState {
                    gitlab_endpoint: args.gitlab_endpoint,
                    token: args.token,
                    web_client:
                    client.clone(),
                    registration_id: args.registration_id
                };
                Ok(state)
            }
            Err(e) => Err(Box::new(e))
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State ) ->  Result<(), ActorProcessingErr> {

    
        //forwrard to client
        if let Some(client) = where_is(BROKER_CLIENT_NAME.to_string()) {
            let client_ref: ActorRef<TcpClientMessage> = ActorRef::from(client);
            
            if let Some(runners) = get_all_elements::<Runner>(&state.web_client, state.token.clone().unwrap_or_default(), format!("{}{}", state.gitlab_endpoint, "/runners/all")).await {
                let data = GitlabData::Runners(runners.clone());
                match send(data, client_ref.clone(), state.registration_id.clone(), RUNNERS_QUEUE_NAME.to_string()) {
                    Ok(_) => {
                        debug!("Successfully sent project data");
                    } Err(e) => todo!()
                }
            }
            else { error!("Couldn't find runners!") }
        } else { error!("Couldn't locate client!") }
        
        myself.stop(Some("FINISHED".to_string()));
        
        Ok(())
    }
    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        _: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        

        Ok(())
    }

}


// #[tokio::main]
// async fn main() -> Result<(), Box<dyn Error> > {
//     let result = create_lock(LOCK_FILE_PATH);
//     match result {
//         Err(e) => panic!("{}", e),
//         Ok(false) => Ok(()),
//         Ok(true) => {
//             info!("running runners task");

//             //publish user id and list of user's projects to queue?
//             let mq_conn = connect_to_rabbitmq().await?;
            
//             //create publish channel

//             let mq_publish_channel = mq_conn.create_channel().await?;

//             //create fresh queue, empty string prompts the server backend to create a random name
//             let _ = mq_publish_channel.queue_declare(RUNNERS_QUEUE_NAME,QueueDeclareOptions::default() , FieldTable::default()).await?;

//             //bind queue to exchange so it sends messages where we need them
//             mq_publish_channel.queue_bind(RUNNERS_QUEUE_NAME, GITLAB_EXCHANGE_STR, RUNNERS_ROUTING_KEY, QueueBindOptions::default(), FieldTable::default()).await?;

//             //poll gitlab for available users
//             let gitlab_token = get_gitlab_token();
//             let service_endpoint = get_gitlab_endpoint();

//             let web_client = helpers::helpers::web_client();
            

            
//             let _ = mq_conn.close(0, "closed").await?;

//             std::fs::remove_file(LOCK_FILE_PATH).expect("Error deleting lock file");
            
//             Ok(())
//         }
//     }
// }
