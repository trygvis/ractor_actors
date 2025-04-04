// Copyright (c) Sean Lawlor
//
// This source code is licensed under both the MIT license found in the
// LICENSE-MIT file in the root directory of this source tree.

extern crate ractor_actors;

use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort};
use ractor_actors::net::tcp::frame_reader::{Frame, FrameReader};
use ractor_actors::net::tcp::listener::*;
use ractor_actors::net::tcp::session::*;
use ractor_actors::net::tcp::stream::*;
use ractor_actors::watchdog;
use ractor_actors::watchdog::TimeoutStrategy;
use std::error::Error;
use std::ops::{BitXor, BitXorAssign};
use std::str::FromStr;

struct MyServer;

struct MyServerArgs {
    /// Controls if the watchdog is used
    watchdog: bool,

    /// The port to listen on
    port: NetworkPort,
}

#[cfg_attr(feature = "async-trait", async_trait::async_trait)]
impl Actor for MyServer {
    type Msg = ();
    type State = ();
    type Arguments = MyServerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        MyServerArgs { watchdog, port }: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let acceptor = MyServerSocketAcceptor { watchdog };

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

        Ok(())
    }
}

struct MyServerSocketAcceptor {
    watchdog: bool,
}

#[cfg_attr(feature = "async-trait", async_trait::async_trait)]
impl SessionAcceptor for MyServerSocketAcceptor {
    async fn new_session(&self, stream: NetworkStream) -> Result<(), ActorProcessingErr> {
        tracing::info!("New connection: {}", stream.peer_addr());
        Actor::spawn(
            Some(format!("MySession-{}", stream.peer_addr().port())),
            MySession {},
            MySessionArgs {
                watchdog: self.watchdog,
                stream,
            },
        )
        .await?;

        Ok(())
    }
}

struct MySession;

enum MySessionMsg {
    Frame(Vec<u8>),
}

struct MySessionState {
    watchdog: bool,
    session: ActorRef<TcpSessionMessage>,
    info: NetworkStreamInfo,
}

struct MySessionArgs {
    watchdog: bool,
    stream: NetworkStream,
}

#[cfg_attr(feature = "async-trait", async_trait::async_trait)]
impl Actor for MySession {
    type Msg = MySessionMsg;
    type State = MySessionState;
    type Arguments = MySessionArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        Self::Arguments { watchdog, stream }: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let info = stream.info();

        tracing::info!("New session: {}", stream.peer_addr());

        let port = OutputPort::default();
        port.subscribe(myself.clone(), |frame: Frame| {
            Some(MySessionMsg::Frame(frame))
        });
        let receiver = FrameReader::new(port);

        let session = TcpSession::spawn_linked(receiver, stream, myself.get_cell()).await?;

        if watchdog {
            watchdog::register(
                myself.get_cell(),
                ractor::concurrency::Duration::from_secs(3),
                TimeoutStrategy::Stop,
            )
                .await?;
        }

        Ok(Self::State {
            watchdog,
            session,
            info,
        })
    }

    async fn post_stop(
        &self,
        _: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::info!("Stopping connection for {}", state.info.peer_addr);

        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if state.watchdog {
            watchdog::ping(state.session.get_id()).await?;
        }

        match message {
            // Self::Msg::RawPacket(packet) => {
            //     tracing::info!("Got packet: {:?}", packet);
            //
            //     let reply: Vec<u8> = packet.iter().map(|x| x.bitxor(0xff)).collect();
            //
            //     let _ = state.session.cast(TcpSessionMessage::Send(reply))?;
            //
            //     Ok(())
            // }
            Self::Msg::Frame(mut frame) => {
                tracing::info!("Got frame of size {}: {:?}", frame.len(), frame);

                let header = (frame.len() as u64).to_be_bytes().to_vec();

                // Invert all the bits and reply with the same message.
                frame.iter_mut().for_each(|b| b.bitxor_assign(0xff));

                let _ = state.session.cast(TcpSessionMessage::Send(header))?;
                let _ = state.session.cast(TcpSessionMessage::Send(frame))?;

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
    tracing::info!("watchdog: {}", watchdog);

    let (system_ref, _) = Actor::spawn(None, MyServer {}, MyServerArgs { watchdog, port }).await?;

    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl-C");

    tracing::info!("fin.");

    system_ref.stop_and_wait(None, None).await?;

    Ok(())
}
