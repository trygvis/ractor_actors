use crate::net::tcp::session::{Packet, PacketReceiver};
use bytes::{Buf, BytesMut};
use ractor::{ActorProcessingErr, OutputPort};

#[derive(Clone)]
pub struct Frame(pub Vec<u8>);

pub struct FrameReader {
    receiver: OutputPort<Frame>,
    buf: BytesMut,
    len: Option<usize>,
}

impl FrameReader {
    pub fn new(receiver: OutputPort<Frame>) -> Self {
        FrameReader {
            receiver,
            buf: BytesMut::default(),
            len: None,
        }
    }

    fn process(&mut self) -> (bool, Option<BytesMut>) {
        match self.len {
            None => {
                if self.buf.len() < 8 {
                    return (false, None);
                }

                let sz = self.buf.get_u64();

                if sz > usize::MAX as u64 {
                    panic!("packet too big for usize")
                }

                self.len = Some(sz as usize);

                (true, None)
            }
            Some(len) => {
                if self.buf.len() >= len {
                    let data = self.buf.split_to(len);
                    (true, Some(data))
                } else {
                    (false, None)
                }
            }
        }
    }
}

#[cfg_attr(feature = "async-trait", ractor::async_trait)]
impl PacketReceiver for FrameReader {
    async fn packet_ready(
        &mut self,
        packet: Packet,
    ) -> Result<(), ActorProcessingErr> {
        self.buf.extend(packet);

        loop {
            let (more, frame) = self.process();

            if let Some(frame) = frame {
                self.receiver.send(Frame(frame.to_vec()))
            }

            if !more {
                break;
            }
        }

        Ok(())
    }
}
