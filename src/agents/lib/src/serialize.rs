use ractor::{Actor, ActorProcessingErr, ActorRef, Message};
use rkyv::{Deserialize, Infallible, Serialize};
use std::marker::PhantomData;

/// Message types handled by the SerializerActor
// #[derive(Debug, Clone)]
// pub enum SerializerMessage<T: Serialize + for<'a> Deserialize<'a>> {
//     Serialize { input: T },
//     Deserialize { bytes: Vec<u8> },
// }

/// Serializer Definition
/// j
pub struct Serializer;

pub struct SerailizerState;

#[async_trait::async_trait]
impl Actor for Serailizer {
    type Msg = SerializerMessage<T>;
    type State = SerailizerState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _: ActorRef<Self>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        message: Self::Msg,
        _: &ActorRef<Self>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            // SerializerMessage::Serialize { input } => {
            //     // Serialize the input to bytes
            //     let serialized = rkyv::to_bytes::<_, 256>(&input).map_err(|err| {
            //         ActorProcessingErr::Panic(format!("Serialization failed: {:?}", err))
            //     })?;
            //     // Forward the serialized bytes to the TCP client
            //     self.tcp_client.cast(serialized.to_vec())?;
            // }
            SerializerMessage::Deserialize { bytes } => {
                // Deserialize the bytes into the target type
                let archived = unsafe { rkyv::archived_root::<T>(&bytes) };
                let deserialized = archived
                    .deserialize(&mut Infallible)
                    .map_err(|err| {
                        ActorProcessingErr::Panic(format!("Deserialization failed: {:?}", err))
                    })?;
                // Forward the deserialized instance to the dispatcher
                self.dispatcher.cast(deserialized)?;
            }
        }
        Ok(())
    }
}

