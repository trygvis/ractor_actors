// Copyright (c) Sean Lawlor
//
// This source code is licensed under both the MIT license found in the
// LICENSE-MIT file in the root directory of this source tree.

//! TCP session actor which is managing the communication on a single socket.

// TODO: RUSTLS + Tokio : https://github.com/tokio-rs/tls/blob/master/tokio-rustls/examples/server/src/main.rs

use super::stream::{NetworkStream, NetworkStreamInfo, ReaderHalf, WriterHalf};
use bytes::BytesMut;
use ractor::{
    Actor, ActorCell, ActorProcessingErr, ActorRef, DerivedActorRef, SpawnErr, State,
    SupervisionEvent,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// =========================== Session actor =========================== //

pub struct BytesAvailable(pub Vec<BytesMut>);
pub struct SendFrame(pub BytesMut);

/// Represents a bidirectional tcp connection along with send + receive operations
///
/// The [Session] actor supervises two child actors, [FrameReader] and [SessionWriter]. Should
/// either the reader or writer exit, they will terminate the entire session.
pub struct Session<AState: State, AMsg, A>
where
    A: Actor<Msg = AMsg, State = AState, Arguments = ReaderHalf>,
{
    handler: DerivedActorRef<BytesAvailable>,
    reader_factory: Arc<
        dyn Fn(
                DerivedActorRef<BytesAvailable>,
            ) -> Pin<Box<dyn Future<Output = Result<A, ActorProcessingErr>> + Send>>
            + Send
            + Sync,
    >,
    pub info: NetworkStreamInfo,
}

impl From<SendFrame> for SessionMessage {
    fn from(SendFrame(frame): SendFrame) -> Self {
        SessionMessage::Send(frame)
    }
}

impl TryFrom<SessionMessage> for SendFrame {
    type Error = ();

    fn try_from(value: SessionMessage) -> Result<Self, Self::Error> {
        match value {
            SessionMessage::Send(frame) => Ok(SendFrame(frame)),
            _ => Err(()),
        }
    }
}

impl From<BytesAvailable> for SessionMessage {
    fn from(BytesAvailable(chunks): BytesAvailable) -> Self {
        SessionMessage::FrameAvailable(chunks)
    }
}

impl TryFrom<SessionMessage> for BytesAvailable {
    type Error = ();

    fn try_from(value: SessionMessage) -> Result<Self, Self::Error> {
        match value {
            SessionMessage::FrameAvailable(chunks) => Ok(BytesAvailable(chunks)),
            _ => Err(()),
        }
    }
}

impl<AState, AMsg, A> Session<AState, AMsg, A>
where
    AState: State,
    AMsg: 'static,
    A: Actor<Msg = AMsg, State = AState, Arguments = ReaderHalf>,
{
    pub async fn spawn_linked(
        handler: DerivedActorRef<BytesAvailable>,
        stream: NetworkStream,
        supervisor: ActorCell,
        reader_factory: Box<
            dyn Fn(
                    DerivedActorRef<BytesAvailable>,
                )
                    -> Pin<Box<dyn Future<Output = Result<A, ActorProcessingErr>> + Send>>
                + Send
                + Sync
                + 'static,
        >,
    ) -> Result<ActorRef<SessionMessage>, SpawnErr> {
        match Actor::spawn_linked(
            Some(stream.peer_addr().to_string()),
            Session {
                handler,
                info: stream.info(),
                reader_factory: Arc::new(reader_factory),
            },
            stream,
            supervisor,
        )
        .await
        {
            Err(err) => {
                tracing::error!("Failed to spawn session writer actor: {err}");
                Err(err)
            }
            Ok((a, _)) => {
                // return the actor handle
                Ok(a)
            }
        }
    }
}

/// The connection messages
pub enum SessionMessage {
    /// Send a message over the channel
    Send(BytesMut),

    /// A frame was received on the channel
    FrameAvailable(Vec<BytesMut>),
}

/// The session's state
pub struct SessionState {
    info: NetworkStreamInfo,
    writer: ActorRef<SessionWriterMessage>,
    reader: ActorCell,
}

#[cfg_attr(feature = "async-trait", ractor::async_trait)]
impl<AState, AMsg: 'static, A> Actor for Session<AState, AMsg, A>
where
    AState: State,
    A: Actor<Msg = AMsg, State = AState, Arguments = ReaderHalf>,
{
    type Msg = SessionMessage;
    type State = SessionState;
    type Arguments = NetworkStream;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        stream: NetworkStream,
    ) -> Result<Self::State, ActorProcessingErr> {
        let info = stream.info();

        let (read, write) = stream.into_split();

        // let (read, write) = stream.into_split();
        // spawn writer + reader child actors
        let (writer, _) = Actor::spawn_linked(
            Some(format!("{}-rw", myself.get_name().unwrap_or_default())),
            SessionWriter,
            write,
            myself.get_cell(),
        )
        .await?;
        let handler = (self.reader_factory)(myself.clone().get_derived()).await?;

        let (reader, _) = Actor::spawn(
            Some(format!("{}-rd", myself.get_name().unwrap_or_default())),
            handler,
            read,
        )
        .await?;

        reader.link(myself.get_cell());

        Ok(Self::State {
            info,
            writer,
            reader: reader.get_cell(),
        })
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::info!("TCP Session closed for {}", self.info.peer_addr);
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            Self::Msg::Send(msg) => {
                // tracing::debug!(
                //     "SEND: {} -> {} - '{msg:?}'",
                //     state.info.local_addr,
                //     state.info.peer_addr
                // );
                let _ = state.writer.cast(SessionWriterMessage::WriteFrame(msg));
            }
            Self::Msg::FrameAvailable(msg) => {
                // tracing::debug!(
                //     "RECEIVE {} <- {} - '{msg:?}'",
                //     state.info.local_addr,
                //     state.info.peer_addr,
                // );
                let _ = self.handler.cast(BytesAvailable(msg));
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // sockets open, they close, the world goes round... If a reader or writer exits for any reason, we'll start the shutdown procedure
        // which requires that all actors exit
        match message {
            SupervisionEvent::ActorFailed(actor, panic_msg) => {
                if actor.get_id() == state.reader.get_id() {
                    tracing::error!("TCP Session's reader panicked with '{panic_msg}'");
                } else if actor.get_id() == state.writer.get_id() {
                    tracing::error!("TCP Session's writer panicked with '{panic_msg}'");
                } else {
                    tracing::error!("TCP Session received a child panic from an unknown child actor ({}) - '{panic_msg}'", actor.get_id());
                }
                myself.stop_children(Some("session_stop_panic".to_string()));
                myself.stop(Some("child_panic".to_string()));
            }
            SupervisionEvent::ActorTerminated(actor, _, exit_reason) => {
                if actor.get_id() == state.reader.get_id() {
                    tracing::debug!("TCP Session's reader exited");
                } else if actor.get_id() == state.writer.get_id() {
                    tracing::debug!("TCP Session's writer exited");
                } else {
                    tracing::warn!("TCP Session received a child exit from an unknown child actor ({}) - '{exit_reason:?}'", actor.get_id());
                }
                myself.stop_children(Some("session_stop_terminated".to_string()));
                myself.stop(Some("child_terminate".to_string()));
            }
            _ => {
                // all ok
            }
        }
        Ok(())
    }
}

// =========================== Session writer =========================== //

struct SessionWriter;

struct SessionWriterState {
    writer: Option<WriterHalf>,
}

enum SessionWriterMessage {
    /// Write a frame over the wire
    WriteFrame(BytesMut),
}

#[cfg_attr(feature = "async-trait", ractor::async_trait)]
impl Actor for SessionWriter {
    type Msg = SessionWriterMessage;
    type Arguments = WriterHalf;
    type State = SessionWriterState;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        writer: WriterHalf,
    ) -> Result<Self::State, ActorProcessingErr> {
        // OK we've established connection, now we can process requests

        Ok(Self::State {
            writer: Some(writer),
        })
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // drop the channel to close it should we be exiting
        drop(state.writer.take());
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SessionWriterMessage::WriteFrame(msg) if state.writer.is_some() => {
                if let Some(stream) = &mut state.writer {
                    if let WriterHalf::Regular(w) = stream {
                        w.writable().await?;
                    }

                    if let Err(write_err) = stream.write_u64(msg.len() as u64).await {
                        tracing::warn!("Error writing to the stream '{}'", write_err);
                    } else {
                        tracing::trace!("Wrote length, writing payload (len={})", msg.len());
                        // now send the frame
                        if let Err(write_err) = stream.write_all(&msg).await {
                            tracing::warn!("Error writing to the stream '{}'", write_err);
                            myself.stop(Some("channel_closed".to_string()));
                            return Ok(());
                        }
                        // flush the stream
                        stream.flush().await?;
                    }
                }
            }
            _ => {
                // no-op, wait for next send request
            }
        }
        Ok(())
    }
}
