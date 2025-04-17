use crate::watchdog::{
    r#impl::Watchdog,
    TimeoutStrategy, WatchdogMsg,
    WatchdogMsg::{Register, Stats},
    WatchdogStats,
};
use ractor::*;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;
use tokio::time::sleep;
use tracing::info;

// Testing the Watchdog actor is a bit complicated since it uses a global instance. Thus, these
// tests should instantiate the implementation directly so they all run in isolation.

// This test however is special and uses the public interface. It can be the only one that does
// this, because the stats would be messed up by any other test running concurrently.
#[concurrency::test]
async fn test_stats() {
    use crate::watchdog;

    async fn get_stats() -> WatchdogStats {
        watchdog::stats().await.unwrap()
    }

    struct MyActor;
    impl Default for MyActor {
        fn default() -> Self {
            Self {}
        }
    }

    #[cfg_attr(feature = "async-trait", ractor::async_trait)]
    impl Actor for MyActor {
        type Msg = ();
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _: ActorRef<Self::Msg>,
            _: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }
    }

    // Reset the stats
    watchdog::reset_stats().await.unwrap();
    assert_eq!(get_stats().await, stats(0, 0, 0, 0));

    // Create and register an actor
    let (actor, _) = spawn::<MyActor>(()).await.unwrap();

    watchdog::register(
        actor.get_cell(),
        Duration::from_millis(100),
        TimeoutStrategy::Stop,
    )
        .await
        .unwrap();

    // Ok, this is mainly to get closer to 100% on the test coverage in mod.rs
    watchdog::ping(actor.get_id()).await.unwrap();

    assert_eq!(get_stats().await, stats(1, 1, 0, 0));

    // Unregister the actor
    watchdog::unregister(actor.get_cell()).await.unwrap();

    assert_eq!(get_stats().await, stats(1, 0, 0, 0));

    // Register it again
    watchdog::register(
        actor.get_cell(),
        Duration::from_millis(100),
        TimeoutStrategy::Stop,
    )
        .await
        .unwrap();

    // Wait for it to be stopped
    sleep(Duration::from_millis(150)).await;

    assert_eq!(get_stats().await, stats(2, 0, 0, 1));

    let _ = actor.stop_and_wait(None, None).await;

    // Try again with Kill strategy

    let (actor, _) = spawn::<MyActor>(()).await.unwrap();

    watchdog::register(
        actor.get_cell(),
        Duration::from_millis(100),
        TimeoutStrategy::Kill,
    )
        .await
        .unwrap();

    // Wait for it to be killed
    sleep(Duration::from_millis(150)).await;

    assert_eq!(get_stats().await, stats(3, 0, 1, 1));

    let _ = actor.stop_and_wait(None, None).await;
}

fn stats(registered: usize, active: usize, kills: usize, stops: usize) -> WatchdogStats {
    WatchdogStats {
        registered,
        active,
        kills,
        stops,
    }
}

#[concurrency::test]
async fn test_actor() {
    static HANDLE: AtomicBool = AtomicBool::new(false);
    static POST_STOP: AtomicBool = AtomicBool::new(false);

    struct MyActor;

    #[cfg_attr(feature = "async-trait", async_trait::async_trait)]
    impl Actor for MyActor {
        type Msg = String;
        type State = ActorRef<WatchdogMsg>;
        type Arguments = ActorRef<WatchdogMsg>;

        async fn pre_start(
            &self,
            myself: ActorRef<Self::Msg>,
            watchdog: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            cast!(
                watchdog.clone(),
                Register(
                    myself.get_cell(),
                    Duration::from_millis(500),
                    TimeoutStrategy::Kill
                )
            )?;

            myself.send_after(Duration::from_millis(400), || "hello".to_string());

            Ok(watchdog)
        }

        async fn handle(
            &self,
            myself: ActorRef<Self::Msg>,
            msg: Self::Msg,
            state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            info!("handle() msg={}", msg);
            HANDLE.store(true, SeqCst);
            cast!(
                state,
                Register(
                    myself.get_cell(),
                    Duration::from_millis(500),
                    TimeoutStrategy::Kill
                )
            )
            .map_err(|e| ActorProcessingErr::from(e))
        }
    }

    info!("starting");
    let (watchdog, watchdog_handle) = Actor::spawn(None, Watchdog, ()).await.unwrap();
    info!("watchdog started");

    info!("starting my_actor");
    let (my_actor, my_actor_handle) = Actor::spawn(None, MyActor, watchdog.clone()).await.unwrap();
    info!("my_actor started");

    sleep(Duration::from_millis(100)).await;

    assert_eq!(false, HANDLE.load(SeqCst));
    assert_eq!(ActorStatus::Running, my_actor.get_status());

    sleep(Duration::from_millis(3000)).await;

    assert_eq!(true, HANDLE.load(SeqCst));
    assert_eq!(false, POST_STOP.load(SeqCst));
    assert_eq!(ActorStatus::Stopped, my_actor.get_status());
    let stats = watchdog
        .call(|port| Stats(port), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(1, stats.kills);

    my_actor_handle.await.unwrap();

    watchdog.stop(None);

    watchdog_handle.await.unwrap();
}
