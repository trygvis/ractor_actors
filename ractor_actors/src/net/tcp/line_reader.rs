use super::stream::ReaderHalf;
use crate::net::tcp::session::BytesAvailable;
use ractor::{Actor, ActorProcessingErr, ActorRef, DerivedActorRef};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};

pub struct LineReader {
    pub session: DerivedActorRef<BytesAvailable>,
}

pub struct LineReaderState {
    reader: Lines<BufReader<ReaderHalf>>,
}

#[cfg_attr(feature = "async-trait", ractor::async_trait)]
impl Actor for LineReader {
    type Msg = ();
    type State = LineReaderState;
    type Arguments = ReaderHalf;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        reader: ReaderHalf,
    ) -> Result<Self::State, ActorProcessingErr> {
        let _ = myself.cast(());

        let reader = BufReader::new(reader);
        let reader = reader.lines();

        Ok(Self::State { reader })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        _message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match state.reader.next_line().await? {
            Some(line) => {
                self.session
                    .cast(BytesAvailable(vec![line.as_bytes().into()]))?;

                myself.cast(())?;
            }
            None => {
                myself.stop(Some("channel_closed".to_string()));
            }
        }

        Ok(())
    }
}
