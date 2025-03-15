use bytes::BytesMut;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use ractor_actors::net::tcp::listener::*;
use ractor_actors::net::tcp::separator_reader::*;
use ractor_actors::net::tcp::session::*;
use ractor_actors::net::tcp::stream::*;
use std::error::Error;
use std::str::FromStr;

struct ChatServer {}
enum ChatServerMsg {}

struct ChatServerState {}

impl Actor for ChatServer {
    type Msg = ChatServerMsg;
    type State = ChatServerState;
    type Arguments = NetworkPort;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        port: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let _ = myself
            .spawn_linked(
                Some(format!("listener-{}", port)),
                Listener::new(
                    port,
                    IncomingEncryptionMode::Raw,
                    move |stream| async move {
                        tracing::info!("New connection: {}", stream.peer_addr());
                        Actor::spawn(
                            Some(format!("MySession-{}", stream.peer_addr().port())),
                            ChatSession {},
                            stream,
                        )
                        .await
                        .map_err(|e| ActorProcessingErr::from(e))?;
                        Ok(())
                    },
                ),
                (),
            )
            .await?;

        Ok(ChatServerState {})
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        _message: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

struct ChatSession {}
enum ChatSessionMsg {
    ReadLine(Vec<BytesMut>),
}

impl From<BytesAvailable> for ChatSessionMsg {
    fn from(BytesAvailable(frame): BytesAvailable) -> Self {
        Self::ReadLine(frame)
    }
}

impl TryFrom<ChatSessionMsg> for BytesAvailable {
    type Error = ();

    fn try_from(msg: ChatSessionMsg) -> Result<Self, Self::Error> {
        match msg {
            ChatSessionMsg::ReadLine(frame) => {
                Ok(BytesAvailable(frame))
            }
        }
    }
}
struct ChatSessionState {}

impl Actor for ChatSession {
    type Msg = ChatSessionMsg;
    type State = ChatSessionState;
    type Arguments = NetworkStream;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        stream: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let _session = Session::spawn_linked(
            myself.get_derived(),
            stream,
            myself.get_cell(),
            Box::new(|session| {
                Box::pin(async {
                    Ok(SeparatorReader {
                        session,
                        separator: '\n' as u8,
                    })
                })
            }),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        Ok(ChatSessionState {})
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        _message: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

fn init_logging() {
    use std::io::stderr;
    use std::io::IsTerminal;
    use tracing::level_filters::LevelFilter;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::DEBUG.into())
        .from_env_lossy();

    let fmt = tracing_subscriber::fmt::Layer::default()
        .with_ansi(stderr().is_terminal())
        .with_writer(stderr)
        .event_format(tracing_subscriber::fmt::format().compact())
        // .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
        // .fmt_fields(GlogFields::default().compact())
        ;

    let subscriber = Registry::default().with(filter).with(fmt);
    tracing::subscriber::set_global_default(subscriber).expect("to set global subscriber");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_logging();

    let s = std::env::var("PORT").unwrap_or("9999".to_string());
    let port = NetworkPort::from_str(&s)?;

    let watchdog = std::env::var("WATCHDOG").unwrap_or("0".to_string()) == "1";

    tracing::info!("Listening on port {}. Watchdog: {}", port, watchdog);

    let (system_ref, _) = Actor::spawn(None, ChatServer {}, port).await?;

    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl-C");

    tracing::info!("fin.");

    system_ref.stop_and_wait(None, None).await?;

    Ok(())
}
