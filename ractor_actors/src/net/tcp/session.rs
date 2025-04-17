// Copyright (c) Sean Lawlor
//
// This source code is licensed under both the MIT license found in the
// LICENSE-MIT file in the root directory of this source tree.

//! TCP session actor which is managing the communication on a single socket.

// TODO: RUSTLS + Tokio : https://github.com/tokio-rs/tls/blob/master/tokio-rustls/examples/server/src/main.rs

use super::stream::{NetworkStream, NetworkStreamInfo, ReaderHalf, WriterHalf};
use ractor::{Actor, ActorCell, ActorProcessingErr, ActorRef, SpawnErr, SupervisionEvent};
use std::marker::PhantomData;
#[cfg(not(feature = "async-trait"))]
use std::marker::Send;
use tokio::io::ErrorKind;
use TcpSessionMessage::*;

/// A frame of data
pub type Frame = Vec<u8>;

/// Represents a receiver of frames of data
#[cfg_attr(feature = "async-trait", ractor::async_trait)]
pub trait FrameReceiver: ractor::State {
    /// Called when a frame is received by the [TcpSession] actor
    #[cfg(not(feature = "async-trait"))]
    fn frame_ready(
        &self,
        f: Frame,
    ) -> impl std::future::Future<Output = Result<(), ActorProcessingErr>> + Send;

    /// Called when a frame is received by the [TcpSession] actor
    #[cfg(feature = "async-trait")]
    async fn frame_ready(&self, f: Frame) -> Result<(), ActorProcessingErr>;
}

/// Supported messages by the [TcpSession] actor
pub enum TcpSessionMessage {
    /// Send a frame over the wire
    Send(Frame),
    /// Received a frame from the `SessionReader` child
    FrameReady(Frame),
}

/// State of a [TcpSession] actor
pub struct TcpSessionState<R>
where
    R: FrameReceiver,
{
    writer: ActorRef<SessionWriterMessage>,
    reader: ActorRef<SessionReaderMessage>,
    receiver: R,
    stream_info: NetworkStreamInfo,
}

/// Startup arguments to starting a tcp session
pub struct TcpSessionStartupArguments<R>
where
    R: FrameReceiver,
{
    /// The callback implementation for received for messages
    pub receiver: R,
    /// The tcp session to create the session upon
    pub tcp_session: NetworkStream,
}

/// A tcp-session management actor
pub struct TcpSession<R>
where
    R: FrameReceiver,
{
    _r: PhantomData<fn() -> R>,
}

impl<R> Default for TcpSession<R>
where
    R: FrameReceiver,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<R> TcpSession<R>
where
    R: FrameReceiver,
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
            TcpSessionStartupArguments{
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
    R: FrameReceiver,
{
    type Msg = TcpSessionMessage;
    type State = TcpSessionState<R>;
    type Arguments = TcpSessionStartupArguments<R>;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let stream_info = args.tcp_session.info();

        let (read, write) = args.tcp_session.into_split();

        // let (read, write) = stream.into_split();
        // spawn writer + reader child actors
        let (writer, _) =
            Actor::spawn_linked(None, SessionWriter, write, myself.get_cell()).await?;
        let (reader, _) = Actor::spawn_linked(
            None,
            SessionReader {
                session: myself.clone(),
            },
            read,
            myself.get_cell(),
        )
        .await?;

        Ok(Self::State {
            writer,
            reader,
            receiver: args.receiver,
            stream_info,
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
            FrameReady(msg) => {
                tracing::trace!(
                    "RECEIVE {} <- {} - '{msg:?}'",
                    state.stream_info.local_addr,
                    state.stream_info.peer_addr,
                );
                state.receiver.frame_ready(msg).await?;
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
                myself.stop_children(Some("session_stop_panic".to_string()));
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

// =========== Tcp stream types + helpers ============ //

struct SessionWriter;

struct SessionWriterState {
    writer: Option<WriterHalf>,
}

enum SessionWriterMessage {
    /// Write an object over the wire
    Write(Frame),
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
                    if let WriterHalf::Regular(w) = stream {
                        w.writable().await?;
                    }

                    if let Err(write_err) = stream.write_u64(msg.len() as u64).await {
                        tracing::warn!("Error writing to the stream '{}'", write_err);
                    } else {
                        tracing::trace!("Wrote length, writing payload (len={})", msg.len());
                        // now send the object
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

// =========== Tcp Session reader ============ //

struct SessionReader {
    session: ActorRef<TcpSessionMessage>,
}

/// The session connection messages
pub enum SessionReaderMessage {
    /// Wait for an object from the stream
    WaitForFrame,

    /// Read next object off the stream
    ReadFrame(u64),
}

struct SessionReaderState {
    reader: Option<ReaderHalf>,
}

#[cfg_attr(feature = "async-trait", ractor::async_trait)]
impl Actor for SessionReader {
    type Msg = SessionReaderMessage;
    type State = SessionReaderState;
    type Arguments = ReaderHalf;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        reader: ReaderHalf,
    ) -> Result<Self::State, ActorProcessingErr> {
        // start waiting for the first object on the network
        let _ = myself.cast(SessionReaderMessage::WaitForFrame);
        Ok(Self::State {
            reader: Some(reader),
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
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            Self::Msg::WaitForFrame if state.reader.is_some() => {
                if let Some(stream) = &mut state.reader {
                    match stream.read_u64().await {
                        Ok(length) => {
                            tracing::trace!("Payload length message ({}) received", length);
                            let _ = myself.cast(SessionReaderMessage::ReadFrame(length));
                            return Ok(());
                        }
                        Err(err) if err.kind() == ErrorKind::UnexpectedEof => {
                            tracing::trace!("Error (EOF) on stream");
                            // EOF, close the stream by dropping the stream
                            drop(state.reader.take());
                            myself.stop(Some("channel_closed".to_string()));
                        }
                        Err(_other_err) => {
                            tracing::trace!("Error ({:?}) on stream", _other_err);
                            // some other TCP error, more handling necessary
                        }
                    }
                }

                let _ = myself.cast(SessionReaderMessage::WaitForFrame);
            }
            Self::Msg::ReadFrame(length) if state.reader.is_some() => {
                if let Some(stream) = &mut state.reader {
                    match stream.read_n_bytes(length as usize).await {
                        Ok(buf) => {
                            tracing::trace!("Payload of length({}) received", buf.len());
                            // NOTE: Our implementation writes 2 messages when sending something over the wire, the first
                            // is exactly 8 bytes which constitute the length of the payload message (u64 in big endian format),
                            // followed by the payload. This tells our TCP reader how much data to read off the wire

                            self.session.cast(FrameReady(buf))?;
                        }
                        Err(err) if err.kind() == ErrorKind::UnexpectedEof => {
                            // EOF, close the stream by dropping the stream
                            drop(state.reader.take());
                            myself.stop(Some("channel_closed".to_string()));
                            return Ok(());
                        }
                        Err(_other_err) => {
                            // TODO: some other TCP error, more handling necessary
                        }
                    }
                }

                // we've read the object, now wait for next object
                let _ = myself.cast(SessionReaderMessage::WaitForFrame);
            }
            _ => {
                // no stream is available, keep looping until one is available
                let _ = myself.cast(SessionReaderMessage::WaitForFrame);
            }
        }
        Ok(())
    }
}
