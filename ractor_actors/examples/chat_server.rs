use ractor::{Actor, ActorCell, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent};
use ractor_actors::net::tcp::line_reader::*;
use ractor_actors::net::tcp::listener::*;
use ractor_actors::net::tcp::session::*;
use ractor_actors::net::tcp::stream::*;
use std::collections::HashMap;
use std::error::Error;
use std::str::FromStr;

type Nick = String;

struct ChatServer {}

enum ChatServerMsg {
    Join(Nick, ActorRef<ChatSessionMsg>),
    Msg(Nick, String),
}

struct ChatServerState {
    users: HashMap<ActorCell, User>,
}

impl ChatServerState {
    pub(crate) fn broadcast(&self, msg: ChatSessionMsg) {
        self.users.values().for_each(|user| {
            let _ = user.actor.cast(msg.clone());
        });
    }
}

struct User {
    nick: String,
    actor: ActorRef<ChatSessionMsg>,
}

impl Actor for ChatServer {
    type Msg = ChatServerMsg;
    type State = ChatServerState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(Self::State {
            users: HashMap::new(),
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ChatServerMsg::Join(nick, actor) => {
                myself.monitor(actor.get_cell());

                state.broadcast(ChatSessionMsg::Join(nick.clone()));

                state.users.insert(actor.get_cell(), User { nick, actor });

                Ok(())
            }
            ChatServerMsg::Msg(nick, msg) => {
                state.broadcast(ChatSessionMsg::Msg(nick, msg));

                Ok(())
            }
        }
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisionEvent::ActorTerminated(actor, _, _)
            | SupervisionEvent::ActorFailed(actor, _) => {
                if let Some(user) = state.users.remove(&actor) {
                    state.broadcast(ChatSessionMsg::Part(user.nick.clone()));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

struct ChatListener {}
enum ChatListenerMsg {}

struct ChatListenerState {}

impl Actor for ChatListener {
    type Msg = ChatListenerMsg;
    type State = ChatListenerState;
    type Arguments = (ActorRef<ChatServerMsg>, NetworkPort);

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        (server, port): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let acceptor = ChatSessionAcceptor { server };

        let _ = myself
            .spawn_linked(
                Some(format!("listener-{}", port)),
                Listener::new(),
                ListenerStartupArgs {
                    port,
                    encryption: IncomingEncryptionMode::Raw,
                    acceptor,
                },
            )
            .await?;

        Ok(Self::State {})
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

struct ChatSessionAcceptor {
    server: ActorRef<ChatServerMsg>,
}

impl SessionAcceptor for ChatSessionAcceptor {
    async fn new_session(&self, stream: NetworkStream) -> Result<(), ActorProcessingErr> {
        tracing::info!("New connection: {}", stream.peer_addr());
        Actor::spawn(
            Some(format!("ChatSession-{}", stream.peer_addr().port())),
            ChatSession {
                server: self.server.clone(),
            },
            stream,
        )
        .await
        .map_err(|e| ActorProcessingErr::from(e))?;
        Ok(())
    }
}

struct ChatSession {
    server: ActorRef<ChatServerMsg>,
}

#[derive(Clone)]
enum ChatSessionMsg {
    ReadLine(String),
    Join(String),
    Part(String),
    Msg(Nick, String),
}

struct ChatSessionState {
    session: ActorRef<TcpSessionMessage>,
    nick: Option<String>,
}

#[allow(dead_code)]
impl ChatSessionState {
    fn send_str(&mut self, string: &str) -> Result<(), ActorProcessingErr> {
        self.session
            .cast(TcpSessionMessage::Send(string.into()))
            .map_err(ActorProcessingErr::from)
    }

    fn send_str_nl(&mut self, string: &str) -> Result<(), ActorProcessingErr> {
        self.send_str(string)?;
        self.send_nl()
    }

    fn send_line(&mut self, string: String) -> Result<(), ActorProcessingErr> {
        self.session
            .cast(TcpSessionMessage::Send(string.as_bytes().into()))
            .map_err(ActorProcessingErr::from)
    }

    fn send_line_nl(&mut self, string: String) -> Result<(), ActorProcessingErr> {
        self.send_line(string)?;
        self.send_nl()
    }

    fn send_nl(&mut self) -> Result<(), ActorProcessingErr> {
        self.session
            .cast(TcpSessionMessage::Send("\r\n".into()))
            .map_err(ActorProcessingErr::from)
    }
}

impl Actor for ChatSession {
    type Msg = ChatSessionMsg;
    type State = ChatSessionState;
    type Arguments = NetworkStream;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        stream: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let port = OutputPort::default();
        port.subscribe(myself.clone(), |line| Some(ChatSessionMsg::ReadLine(line)));
        let receiver = LineReader::new(port);

        let session = TcpSession::spawn_linked(receiver, stream, myself.get_cell())
            .await
            .map_err(ActorProcessingErr::from)?;

        let mut state = ChatSessionState {
            session,
            nick: None,
        };

        state.send_str("Enter your nick: ")?;

        Ok(state)
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ChatSessionMsg::ReadLine(line) => match state.nick.clone() {
                None => {
                    state.send_line_nl(format!("Welcome {}!", &line))?;
                    self.server
                        .cast(ChatServerMsg::Join(line.clone(), myself))?;
                    state.nick = Some(line);
                    Ok(())
                }
                Some(nick) => {
                    self.server.cast(ChatServerMsg::Msg(nick, line))?;
                    Ok(())
                }
            },
            ChatSessionMsg::Join(nick) => state.send_line_nl(format!("join: {}", nick)),
            ChatSessionMsg::Part(nick) => state.send_line_nl(format!("part: {}", nick)),
            ChatSessionMsg::Msg(nick, msg) => {
                if nick != state.nick.clone().unwrap_or_default() {
                    state.send_line_nl(format!("{}: {}", nick, msg,))?;
                }

                Ok(())
            }
        }
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

    let (server, _) = Actor::spawn(None, ChatServer {}, ()).await?;
    let (listener, _) = Actor::spawn(None, ChatListener {}, (server.clone(), port)).await?;

    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl-C");

    tracing::info!("fin.");

    listener.stop_and_wait(None, None).await?;
    server.stop_and_wait(None, None).await?;

    Ok(())
}
