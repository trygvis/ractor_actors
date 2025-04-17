use crate::watchdog::{
    r#impl::Watchdog,
    WatchdogMsg,
    WatchdogMsg::{Register, Stats},
    WatchdogStats,
    {TimeoutStrategy, TimeoutStrategy::*},
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
    assert_eq!(get_stats().await, new_stats(0, 0, 0, 0));

    // Create and register an actor
    let (actor, _) = spawn::<MyActor>(()).await.unwrap();

    watchdog::register(actor.get_cell(), Duration::from_millis(100), Stop)
        .await
        .unwrap();

    // Ok, this is mainly to get closer to 100% on the test coverage in mod.rs
    watchdog::ping(actor.get_id()).await.unwrap();

    assert_eq!(get_stats().await, new_stats(1, 1, 0, 0));

    // Unregister the actor
    watchdog::unregister(actor.get_cell()).await.unwrap();

    assert_eq!(get_stats().await, new_stats(1, 0, 0, 0));

    // Register it again
    watchdog::register(actor.get_cell(), Duration::from_millis(100), Stop)
        .await
        .unwrap();

    // Wait for it to be stopped
    sleep(Duration::from_millis(150)).await;

    assert_eq!(get_stats().await, new_stats(2, 0, 0, 1));

    let _ = actor.stop_and_wait(None, None).await;

    // Try again with Kill strategy

    let (actor, _) = spawn::<MyActor>(()).await.unwrap();

    watchdog::register(actor.get_cell(), Duration::from_millis(100), Kill)
        .await
        .unwrap();

    // Wait for it to be killed
    sleep(Duration::from_millis(150)).await;

    assert_eq!(get_stats().await, new_stats(3, 0, 1, 1));

    let _ = actor.stop_and_wait(None, None).await;
}

fn new_stats(registered: usize, active: usize, kills: usize, stops: usize) -> WatchdogStats {
    WatchdogStats {
        registered,
        active,
        kills,
        stops,
    }
}

async fn register<T>(
    watchdog: &ActorRef<WatchdogMsg>,
    subject: &ActorRef<T>,
    duration_ms: u64,
    timeout_strategy: TimeoutStrategy,
) -> () {
    let duration = Duration::from_millis(duration_ms);
    watchdog
        .cast(Register(subject.get_cell(), duration, timeout_strategy))
        .map_err(ActorProcessingErr::from)
        .unwrap()
}

async fn stats(watchdog: &ActorRef<WatchdogMsg>) -> WatchdogStats {
    watchdog
        .call(|port| Stats(port), None)
        .await
        .unwrap()
        .unwrap()
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
            register(&watchdog, &myself, 500, Kill).await;

            myself.send_after(Duration::from_millis(400), || "hello".to_string());

            Ok(watchdog)
        }

        async fn handle(
            &self,
            myself: ActorRef<Self::Msg>,
            msg: Self::Msg,
            watchdog: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            info!("handle() msg={}", msg);
            HANDLE.store(true, SeqCst);
            register(watchdog, &myself, 500, Kill).await;

            Ok(())
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
    let stats = stats(&watchdog).await;
    assert_eq!(1, stats.kills);

    my_actor_handle.await.unwrap();

    watchdog.stop(None);

    watchdog_handle.await.unwrap();
}

#[concurrency::test]
#[tracing_test::traced_test]
async fn test_double_registration() {
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

    let (watchdog, watchdog_handle) = Actor::spawn(None, Watchdog, ()).await.unwrap();

    let (actor, _) = spawn::<MyActor>(()).await.unwrap();

    // Register the same actor twice, once with a short interval and one with longer
    // Sleep somewhere in between the durations
    // Check that the actor is alive, showing that long once did override the shorter one
    register(&watchdog, &actor, 100, Stop).await;

    register(&watchdog, &actor, 500, Stop).await;

    sleep(Duration::from_millis(250)).await;

    assert_eq!(actor.get_status(), ActorStatus::Running);

    let _ = actor.stop_and_wait(None, None).await;

    watchdog.stop(None);

    watchdog_handle.await.unwrap();
}
