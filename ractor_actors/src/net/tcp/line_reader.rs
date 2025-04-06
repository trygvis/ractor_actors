// Copyright (c) Sean Lawlor
//
// This source code is licensed under both the MIT license found in the
// LICENSE-MIT file in the root directory of this source tree.

//! A [PacketReceiver] that converts the input stream into String lines. Each line is either `\n` or
//! `\r\n` separated.

use crate::net::tcp::session::{Packet, PacketReceiver};
use ractor::{ActorProcessingErr, OutputPort};

// TODO: The buffer could probably benefit from cleaner code and less byte copying by using BytesMut
// as a buffer instead of Vec<u8>.

pub struct LineReader {
    receiver: OutputPort<String>,
    buf: Vec<u8>,
    skip: bool,
}

impl LineReader {
    pub fn new(receiver: OutputPort<String>) -> Self {
        Self {
            receiver,
            buf: Vec::default(),
            skip: false,
        }
    }

    fn process(&mut self) -> (bool, Option<Vec<u8>>) {
        // If we have consumed a \r, we skip the next byte if it is \n.
        if self.skip {
            let next = self.buf.first();
            match next {
                None => {
                    // ... however, we need at least 1 byte to determine that.
                    return (false, None);
                }
                Some(b'\n') => {
                    // The next byte was a \n, so return the current data as a new line and keep on
                    // processing
                    self.buf.drain(0..1);
                    // return (true, None);
                }
                Some(_) => (),
            }
        }

        let pos = self.buf.iter().position(|&b| b == b'\r' || b == b'\n');

        match pos {
            None => (false, None),
            Some(idx) => {
                let next = self.buf.split_off(idx + 1);
                let current = std::mem::replace(&mut self.buf, next);

                let a = current.last();

                // if the last byte is \r, set the skip flag and try again
                if a.is_some_and(|&a| a == b'\r') {
                    self.skip = true;
                }

                (true, Some(current[0..current.len() - 1].to_vec()))
            }
        }
    }
}

#[cfg_attr(feature = "async-trait", ractor::async_trait)]
impl PacketReceiver for LineReader {
    async fn packet_ready(&mut self, packet: Packet) -> Result<(), ActorProcessingErr> {
        self.buf.extend_from_slice(packet.as_slice());

        loop {
            let (more, line) = self.process();

            if let Some(line) = line {
                let string = String::from_utf8(line)?;

                self.receiver.send(string);
            }

            if !more {
                break;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::net::tcp::line_reader::LineReader;
    use ractor::OutputPort;

    // #[ractor::concurrency::test]
    #[test]
    fn line_reader() {
        check(vec![
            ("abc", false, None), //
            ("", false, None),
        ]);

        check(vec![
            ("abc\nxyz\nbleh", true, Some("abc")),
            ("", true, Some("xyz")),
            ("", false, None),
        ]);

        check(vec![
            ("a", false, None),
            ("\n", true, Some("a")),
            ("", false, None),
        ]);

        check(vec![
            ("abc\r\n", true, Some("abc")), //
            ("", false, None),              //
        ]);

        check(vec![
            ("a", false, None),
            ("\r", true, Some("a")),
            ("\n", false, None),
        ]);
    }

    fn check(io: Vec<(&str, bool, Option<&str>)>) {
        let port = OutputPort::default();
        let mut lr = LineReader::new(port);

        let mut idx = 1;
        for (input, more, res) in io {
            lr.buf.extend(input.as_bytes());
            let expected = (more, res.map(|s| s.as_bytes().to_vec()));
            let actual = lr.process();
            assert_eq!(expected, actual, "input #{}: {:0x?}", idx, input);

            idx += 1;
        }
    }
}
