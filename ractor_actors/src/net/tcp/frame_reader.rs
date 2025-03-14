use super::session::SessionMessage;
use super::stream::ReaderHalf;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::io::ErrorKind;

pub struct FrameReader {
    pub session: ActorRef<SessionMessage>,
}

/// The session connection messages
pub enum FrameReaderMessage {
    /// Wait for a frame from the stream
    WaitForFrame,

    /// Read next frame off the stream
    ReadFrame(u64),
}

pub struct FrameReaderState {
    reader: Option<ReaderHalf>,
}

#[cfg_attr(feature = "async-trait", ractor::async_trait)]
impl Actor for FrameReader {
    type Msg = FrameReaderMessage;
    type State = FrameReaderState;
    type Arguments = ReaderHalf;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        reader: ReaderHalf,
    ) -> Result<Self::State, ActorProcessingErr> {
        // start waiting for the first frame on the network
        let _ = myself.cast(FrameReaderMessage::WaitForFrame);
        Ok(Self::State {
            reader: Some(reader),
        })
    }

    // TODO: move this to the Session actor
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
                            tracing::trace!("Payload length message ({length}) received");
                            let _ = myself.cast(FrameReaderMessage::ReadFrame(length));
                            return Ok(());
                        }
                        Err(err) if err.kind() == ErrorKind::UnexpectedEof => {
                            tracing::trace!("Error (EOF) on stream");
                            // EOF, close the stream by dropping the stream
                            drop(state.reader.take());
                            myself.stop(Some("channel_closed".to_string()));
                        }
                        Err(_other_err) => {
                            tracing::trace!("Error ({_other_err:?}) on stream");
                            // some other TCP error, more handling necessary
                        }
                    }
                }

                let _ = myself.cast(FrameReaderMessage::WaitForFrame);
            }
            Self::Msg::ReadFrame(length) if state.reader.is_some() => {
                if let Some(stream) = &mut state.reader {
                    match stream.read_n_bytes(length as usize).await {
                        Ok(buf) => {
                            tracing::trace!("Payload of length({}) received", buf.len());
                            // NOTE: Our implementation writes 2 messages when sending something over the wire, the first
                            // is exactly 8 bytes which constitute the length of the payload message (u64 in big endian format),
                            // followed by the payload. This tells our TCP reader how much data to read off the wire

                            let _ = self.session.cast(SessionMessage::FrameAvailable(buf));
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

                // we've read the frame, now wait for next object
                let _ = myself.cast(FrameReaderMessage::WaitForFrame);
            }
            _ => {
                // no stream is available, keep looping until one is available
                let _ = myself.cast(FrameReaderMessage::WaitForFrame);
            }
        }
        Ok(())
    }
}
