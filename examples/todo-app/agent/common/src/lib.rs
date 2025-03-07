
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use rkyv::rancor::Error;
use rkyv::{Serialize, Deserialize, Archive};
use tracing::{error, info, warn};
use polar::DispatcherMessage;


#[derive(Debug, serde::Deserialize, serde::Serialize, Deserialize, Serialize, Archive, Clone)]
pub struct Todo {
    pub id: u16,
    pub value: String,
    pub done: bool,
}
#[derive(Deserialize, Serialize, Archive, Clone)]
pub enum TodoData {
    Todo(Vec<Todo>),
    OpenApiSpec(String)
}


/// Dispatcher Definition
/// The Dispatcher is a core component of each agent. Its function is to deserialize data from it's binary
/// format into strong rust datatypes, and forward it to any interested actors.
pub struct MessageDispatcher;

pub struct DispatcherState;

//TODO: Make Dispatcher a trait that forces the implementation of some serialization logic>
impl MessageDispatcher {

    /// Helper function - does as the name says.
    /// It's pretty safe to assume whatever message comes in contains data for our example agent.
    /// We can jsut discard any message that don't conform to our expectations as bad data.
    /// The incoming topic string will inform us of which actor is responsible for handling the message.
    pub fn deserailize_and_dispatch(message: Vec<u8>, topic: String) {
        match rkyv::from_bytes::<TodoData, Error>(&message) {
            Ok(message) => {
                if let Some(consumer) = where_is(topic.clone()) {
                    
                    if let Err(e) = consumer.send_message(message) {
                        tracing::warn!("Error forwarding message. {e}");
                    }
                } else {
                    //TODO: Implement DLQ for when consumers aren't present and may return?
                    todo!("Failed to forward message to processor, implement DLQ");;
                }
            }
            Err(err) => warn!("Failed to deserialize message: {:?}", err),
        }
        
    }
}



#[async_trait]
impl Actor for MessageDispatcher {
    type Msg = DispatcherMessage;
    type State = DispatcherState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ()
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(DispatcherState)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
        info!("{myself:?} started");
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            DispatcherMessage::Dispatch { message, topic } => { MessageDispatcher::deserailize_and_dispatch(message, topic); }        
        }
        Ok(())        
    }
}

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
