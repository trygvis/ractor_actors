// Copyright (c) Sean Lawlor
//
// This source code is licensed under both the MIT license found in the
// LICENSE-MIT file in the root directory of this source tree.

//! TCP session actor which is managing the communication on a single socket.

// TODO: RUSTLS + Tokio : https://github.com/tokio-rs/tls/blob/master/tokio-rustls/examples/server/src/main.rs

use super::stream::{NetworkStream, NetworkStreamInfo, ReaderHalf, WriterHalf};
use bytes::BytesMut;
use ractor::{Actor, ActorCell, ActorProcessingErr, ActorRef, SpawnErr, SupervisionEvent};
use std::marker::PhantomData;
#[cfg(not(feature = "async-trait"))]
use std::marker::Send;
use tokio::io::ErrorKind;
use TcpSessionMessage::*;

/// A packet of data
pub type Packet = Vec<u8>;

/// Represents a receiver of packets of data
#[cfg_attr(feature = "async-trait", ractor::async_trait)]
pub trait PacketReceiver: ractor::State {
    /// Called when a packet is received by the [TcpSession] actor
    #[cfg(not(feature = "async-trait"))]
    fn packet_ready(
        &mut self,
        f: Packet,
    ) -> impl std::future::Future<Output = Result<(), ActorProcessingErr>> + Send;

    /// Called when a packet is received by the [TcpSession] actor
    #[cfg(feature = "async-trait")]
    async fn packet_ready(&mut self, f: Packet) -> Result<(), ActorProcessingErr>;
}

/// Supported messages by the [TcpSession] actor
pub enum TcpSessionMessage {
    /// Send a packet over the wire
    Send(Packet),
    /// Received a packet from the `SessionReader` child
    PacketReady(Packet),
}

/// State of a [TcpSession] actor
pub struct TcpSessionState<R>
where
    R: PacketReceiver,
{
    writer: ActorRef<SessionWriterMessage>,
    reader: ActorRef<()>,
    receiver: R,
    stream_info: NetworkStreamInfo,
}

/// Startup arguments to starting a tcp session
pub struct TcpSessionStartupArguments<R>
where
    R: PacketReceiver,
{
    /// The callback implementation for received for messages
    pub receiver: R,
    /// The tcp session to create the session upon
    pub tcp_session: NetworkStream,
}

/// A tcp-session management actor
pub struct TcpSession<R>
where
    R: PacketReceiver,
{
    _r: PhantomData<fn() -> R>,
}

impl<R> Default for TcpSession<R>
where
    R: PacketReceiver,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<R> TcpSession<R>
where
    R: PacketReceiver,
{
    /// Create a new TcpSession actor instance
    pub fn new() -> Self {
        Self { _r: PhantomData }
    }

    pub async fn spawn_linked(
        receiver: R,
        stream: NetworkStream,
        supervisor: ActorCell,
    ) -> Result<ActorRef<TcpSessionMessage>, SpawnErr> {
        match Actor::spawn_linked(
            Some(stream.peer_addr().to_string()),
            TcpSession::new(),
            TcpSessionStartupArguments {
                receiver,
                tcp_session: stream,
            },
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

#[cfg_attr(feature = "async-trait", async_trait::async_trait)]
impl<R> Actor for TcpSession<R>
where
    R: PacketReceiver,
{
    type Msg = TcpSessionMessage;
    type State = TcpSessionState<R>;
    type Arguments = TcpSessionStartupArguments<R>;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let info = args.tcp_session.info();

        let (read, write) = args.tcp_session.into_split();

        // let (read, write) = stream.into_split();
        // spawn writer + reader child actors
        let (writer, _) =
            Actor::spawn_linked(None, SessionWriter, write, myself.get_cell()).await?;

        let (reader, _) = Actor::spawn_linked(
            None,
            SessionReader {},
            SessionReaderArgs {
                reader: read,
                session: myself.clone(),
            },
            myself.get_cell(),
        )
        .await?;

        Ok(Self::State {
            stream_info: info,
            writer,
            reader,
            receiver: args.receiver,
        })
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::info!("TCP Session closed for {}", state.stream_info.peer_addr);
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            Send(msg) => {
                tracing::trace!(
                    "SEND: {} -> {} - '{msg:?}'",
                    state.stream_info.local_addr,
                    state.stream_info.peer_addr
                );
                let _ = state.writer.cast(SessionWriterMessage::Write(msg));
            }
            PacketReady(msg) => {
                tracing::trace!(
                    "RECEIVE {} <- {} - '{msg:?}'",
                    state.stream_info.local_addr,
                    state.stream_info.peer_addr,
                );
                state.receiver.packet_ready(msg).await?;
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
                    tracing::error!("TCP Session's reader panicked with '{}'", panic_msg);
                } else if actor.get_id() == state.writer.get_id() {
                    tracing::error!("TCP Session's writer panicked with '{}'", panic_msg);
                } else {
                    tracing::error!("TCP Session received a child panic from an unknown child actor ({}) - '{}'", actor.get_id(), panic_msg);
                }

                myself
                    .stop_children_and_wait(Some("session_stop_panic".to_string()), None)
                    .await;
                myself.stop(Some("child_panic".to_string()));
            }
            SupervisionEvent::ActorTerminated(actor, _, exit_reason) => {
                if actor.get_id() == state.reader.get_id() {
                    tracing::debug!("TCP Session's reader exited");
                } else if actor.get_id() == state.writer.get_id() {
                    tracing::debug!("TCP Session's writer exited");
                } else {
                    tracing::warn!("TCP Session received a child exit from an unknown child actor ({}) - '{:?}'", actor.get_id(), exit_reason);
                }

                myself
                    .stop_children_and_wait(Some("session_stop_panic".to_string()), None)
                    .await;
                myself.stop(Some("child_panic".to_string()));
            }
            _ => {
                // all ok
            }
        }
        Ok(())
    }
}

// =========== Tcp stream types ============ //

struct SessionWriter;

struct SessionWriterState {
    writer: Option<WriterHalf>,
}

enum SessionWriterMessage {
    /// Write an object over the wire
    Write(Vec<u8>),
}

#[cfg_attr(feature = "async-trait", ractor::async_trait)]
impl Actor for SessionWriter {
    type Msg = SessionWriterMessage;
    type State = SessionWriterState;
    type Arguments = WriterHalf;

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
            SessionWriterMessage::Write(msg) if state.writer.is_some() => {
                if let Some(stream) = &mut state.writer {
                    if let Err(write_err) = stream.write_all(&msg).await {
                        tracing::warn!("Error writing to the stream '{}'", write_err);
                        myself.stop(Some("channel_closed".to_string()));
                        return Ok(());
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

// =========== Tcp Session reader ============ //

struct SessionReader;

struct SessionReaderState {
    reader: Option<ReaderHalf>,
    session: ActorRef<TcpSessionMessage>,
}

struct SessionReaderArgs {
    reader: ReaderHalf,
    session: ActorRef<TcpSessionMessage>,
}

#[cfg_attr(feature = "async-trait", async_trait::async_trait)]
impl Actor for SessionReader {
    type Msg = ();
    type State = SessionReaderState;
    type Arguments = SessionReaderArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        // start waiting for the first object on the network
        let _ = myself.cast(());
        Ok(Self::State {
            reader: Some(args.reader),
            session: args.session,
        })
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // drop the channel to close it should we be exiting
        drop(state.reader.take());
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        _: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        loop {
            if let Some(ref mut reader) = &mut state.reader {
                let mut buf: [u8; 8192] = [0; 8192];
                let sz = reader.read(&mut buf).await;
                match sz {
                    Ok(0) => {
                        myself.stop(Some("channel_closed".to_string()));
                        break;
                    }
                    Err(err) => {
                        tracing::warn!("Error reading from reader: {}", err);
                        break;
                    }
                    Ok(sz) => state
                        .session
                        .cast(PacketReady(buf[0..sz].to_vec()))
                        .map_err(ActorProcessingErr::from)?,
                }
            }
        }

        Ok(())
    }
}
