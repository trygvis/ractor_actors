use super::stream::ReaderHalf;
use bytes::BufMut;
use bytes::BytesMut;
use ractor::{Actor, ActorProcessingErr, ActorRef, DerivedActorRef};
use std::io::ErrorKind;
use std::mem;

const BUFFER_SIZE: usize = 8 * 1024;

pub struct SeparatorReader {
    pub separator: u8,
}

// pub struct SeparatorReaderState {
//     reader: ReaderHalf,
//     buffers: Vec<BytesMut>,
// }

impl SeparatorReader {
    // async fn pre_start(
    //     &self,
    //     myself: ActorRef<Self::Msg>,
    //     reader: ReaderHalf,
    // ) -> Result<Self::State, ActorProcessingErr> {
    //     let _ = myself.cast(());
    //     Ok(Self::State {
    //         reader,
    //         buffers: Vec::new(),
    //     })
    // }

    // async fn handle(
    //     &self,
    //     myself: ActorRef<Self::Msg>,
    //     _message: Self::Msg,
    //     state: &mut Self::State,
    // ) -> Result<(), ActorProcessingErr> {
    //     let mut buf = [0; BUFFER_SIZE];
    //
    //     match state.reader.read(&mut buf).await {
    //         Ok(0) => {
    //             tracing::trace!("EOF");
    //             myself.stop(Some("channel_closed".to_string()));
    //         }
    //         Err(err) if err.kind() == ErrorKind::UnexpectedEof => {
    //             tracing::trace!("EOF");
    //             myself.stop(Some("channel_closed".to_string()));
    //         }
    //         Err(_other_err) => {
    //             tracing::trace!("Error ({_other_err:?}) on stream");
    //             myself.stop(Some("channel_error".to_string()));
    //         }
    //         Ok(length) => {
    //             tracing::trace!("Read {} bytes", length);
    //
    //             match buf[..length].iter().position(|b| *b == self.separator) {
    //                 Some(index) => {
    //                     let (left, right) = buf.split_at(index);
    //                     state.buffers.push(left.into());
    //
    //                     // Drop the separator
    //                     let right = &right[1..];
    //
    //                     // Get the full buffer's array and create a new, empty one.
    //                     let vec = vec![right.into()];
    //                     let buffers = mem::replace(&mut state.buffers, vec);
    //
    //                     let byte_len: usize = buffers.iter().map(|b| b.len()).sum();
    //                     tracing::trace!("Sending {} bytes", byte_len);
    //                     let r = self.session.cast(BytesAvailable(buffers));
    //                     tracing::trace!("Sending {:?} bytes", r);
    //                 }
    //                 None => {
    //                     let mut bs = BytesMut::with_capacity(length);
    //                     bs.put_slice(&buf[..length]); // Zero-copy slice management
    //
    //                     state.buffers.push(bs);
    //                 }
    //             }
    //
    //             let _ = myself.cast(());
    //             return Ok(());
    //         }
    //     }
    //
    //     let _ = myself.cast(());
    //
    //     Ok(())
    // }
}
