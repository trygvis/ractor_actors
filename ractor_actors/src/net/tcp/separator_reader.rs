use super::stream::ReaderHalf;
use crate::net::tcp::session::BytesAvailable;
use bytes::BufMut;
use bytes::BytesMut;
use ractor::{Actor, ActorProcessingErr, ActorRef, DerivedActorRef};
use std::io::ErrorKind;
use std::mem;

const BUFFER_SIZE: usize = 8 * 1024;

pub struct SeparatorReader {
    pub session: DerivedActorRef<BytesAvailable>,
    pub separator: u8,
}

pub enum SeparatorReaderMessage {
    Reading,
}

pub struct SeparatorReaderState {
    reader: Option<ReaderHalf>,
    buffers: Vec<BytesMut>,
}

#[cfg_attr(feature = "async-trait", ractor::async_trait)]
impl Actor for SeparatorReader {
    type Msg = SeparatorReaderMessage;
    type State = SeparatorReaderState;
    type Arguments = ReaderHalf;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        reader: ReaderHalf,
    ) -> Result<Self::State, ActorProcessingErr> {
        let _ = myself.cast(Self::Msg::Reading);
        Ok(Self::State {
            reader: Some(reader),
            buffers: Vec::new(),
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match (message, &mut state.reader) {
            (Self::Msg::Reading, Some(reader)) => {
                let mut buf = [0; BUFFER_SIZE];

                match reader.read(&mut buf).await {
                    Ok(length) => {
                        tracing::trace!("Read {} bytes", length);

                        match buf.iter().position(|b| *b == self.separator) {
                            Some(index) => {
                                let (left, right) = buf.split_at(index);
                                state.buffers.push(left.into());

                                // Get the full buffer's array and create a new, empty one.
                                let vec = vec![right.into()];
                                let buffers = mem::replace(&mut state.buffers, vec);

                                let _ = self.session.cast(BytesAvailable(buffers));
                            }
                            None => {
                                let mut bs = BytesMut::with_capacity(length);
                                bs.put_slice(&buf[..length]); // Zero-copy slice management

                                state.buffers.push(bs);
                            }
                        }

                        let _ = myself.cast(SeparatorReaderMessage::Reading);
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
            _ => {
                // no stream is available, keep looping until one is available
            }
        }

        let _ = myself.cast(SeparatorReaderMessage::Reading);

        Ok(())
    }
}
