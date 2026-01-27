use actor_protocol::{ActorError, SystemEvent};
use futures::stream::StreamExt;
use futures_channel::mpsc;

/// Actor trait for implementing message-driven components
///
/// Actors are independent, stateful components that communicate through
/// message passing. Each actor has its own message queue and processes
/// messages sequentially.
///
/// # Lifecycle
///
/// 1. **init()** - Called once before message processing starts
/// 2. **handle()** - Called for each received message
/// 3. **shutdown()** - Called when the actor is stopping
///
/// # Send Bounds
///
/// On native targets, Actor and Message must be Send to support multi-threaded
/// execution. On WASM (single-threaded), Send is not required, allowing use of
/// Rc, RefCell, and other !Send types.
///
/// # Example
///
/// ```ignore
/// struct MyActor {
///     state: u32,
///     event_tx: mpsc::Sender<SystemEvent>,
/// }
///
/// impl Actor for MyActor {
///     type Message = MyMessage;
///
///     fn name(&self) -> &'static str {
///         "MyActor"
///     }
///
///     async fn init(&mut self) -> Result<(), ActorError> {
///         Ok(())
///     }
///
///     async fn handle(&mut self, msg: Self::Message) -> Result<(), ActorError> {
///         // Process message
///         Ok(())
///     }
/// }
/// ```
#[allow(async_fn_in_trait)]
#[cfg(not(target_arch = "wasm32"))]
pub trait Actor: Send + 'static {
    /// Message type this actor processes
    type Message: Send + 'static;

    /// Actor name (used for logging and debugging)
    fn name(&self) -> &'static str;

    /// Initialize the actor before processing messages
    ///
    /// Called once when the actor starts. Use this to set up resources,
    /// restore state, or perform initial configuration.
    async fn init(&mut self) -> Result<(), ActorError> {
        Ok(())
    }

    /// Handle a single message
    ///
    /// This is called for each message received by the actor.
    async fn handle(&mut self, msg: Self::Message) -> Result<(), ActorError>;

    /// Clean up before shutdown
    ///
    /// Called when the actor is stopping. Use this to close connections,
    /// save state, or release resources.
    async fn shutdown(&mut self) {}

    /// Main actor run loop (provided by runtime)
    ///
    /// This method consumes the actor and runs it to completion.
    /// It handles initialization, message processing, and shutdown.
    ///
    /// # Arguments
    ///
    /// * `rx` - Channel to receive messages from
    /// * `event_tx` - Channel to send events to UI
    async fn run(
        mut self,
        mut rx: mpsc::Receiver<Self::Message>,
        event_tx: mpsc::Sender<SystemEvent>,
    ) where
        Self: Sized,
    {
        // Initialize
        if let Err(e) = self.init().await {
            let _ = event_tx.clone().try_send(SystemEvent::Error {
                message: format!("{} init failed: {}", self.name(), e),
            });
            return;
        }

        #[cfg(debug_assertions)]
        {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&format!("{} started", self.name()).into());
            #[cfg(not(target_arch = "wasm32"))]
            eprintln!("{} started", self.name());
        }

        // Process messages
        while let Some(msg) = rx.next().await {
            if let Err(e) = self.handle(msg).await {
                let _ = event_tx.clone().try_send(SystemEvent::Error {
                    message: format!("{} error: {}", self.name(), e),
                });
            }
        }

        // Shutdown
        self.shutdown().await;

        #[cfg(debug_assertions)]
        {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&format!("{} stopped", self.name()).into());
            #[cfg(not(target_arch = "wasm32"))]
            eprintln!("{} stopped", self.name());
        }
    }
}

/// WASM-specific Actor trait (no Send bound)
///
/// WASM is single-threaded, so Send is not meaningful. This allows actors
/// to use Rc, RefCell, and web_sys types without unsafe Send impls.
#[allow(async_fn_in_trait)]
#[cfg(target_arch = "wasm32")]
pub trait Actor: 'static {
    /// Message type this actor processes
    type Message: 'static;

    /// Actor name (used for logging and debugging)
    fn name(&self) -> &'static str;

    /// Initialize the actor before processing messages
    ///
    /// Called once when the actor starts. Use this to set up resources,
    /// restore state, or perform initial configuration.
    async fn init(&mut self) -> Result<(), ActorError> {
        Ok(())
    }

    /// Handle a single message
    ///
    /// This is called for each message received by the actor.
    /// Messages are processed sequentially in the order received.
    async fn handle(&mut self, msg: Self::Message) -> Result<(), ActorError>;

    /// Clean up resources before shutdown
    ///
    /// Called when the actor is stopping. Use this to close connections,
    /// save state, or release resources.
    async fn shutdown(&mut self) {
        // Default: no cleanup needed
    }

    /// Main actor run loop (provided by runtime)
    ///
    /// This method consumes the actor and runs it to completion.
    /// It handles initialization, message processing, and shutdown.
    ///
    /// # Arguments
    ///
    /// * `rx` - Channel to receive messages from
    /// * `event_tx` - Channel to send events to UI
    async fn run(
        mut self,
        mut rx: mpsc::Receiver<Self::Message>,
        event_tx: mpsc::Sender<SystemEvent>,
    ) where
        Self: Sized,
    {
        // Initialize
        if let Err(e) = self.init().await {
            let _ = event_tx.clone().try_send(SystemEvent::Error {
                message: format!("{} init failed: {}", self.name(), e),
            });
            return;
        }

        #[cfg(debug_assertions)]
        {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&format!("{} started", self.name()).into());
            #[cfg(not(target_arch = "wasm32"))]
            eprintln!("{} started", self.name());
        }

        // Process messages
        while let Some(msg) = rx.next().await {
            if let Err(e) = self.handle(msg).await {
                let _ = event_tx.clone().try_send(SystemEvent::Error {
                    message: format!("{} error: {}", self.name(), e),
                });
            }
        }

        // Shutdown
        self.shutdown().await;

        #[cfg(debug_assertions)]
        {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&format!("{} stopped", self.name()).into());
            #[cfg(not(target_arch = "wasm32"))]
            eprintln!("{} stopped", self.name());
        }
    }
}

/// Helper to spawn an actor in the current runtime
///
/// For WASM, this uses wasm_bindgen_futures::spawn_local
/// For native, this would use tokio::spawn (if async runtime available)
pub fn spawn_actor<A>(actor: A, rx: mpsc::Receiver<A::Message>, event_tx: mpsc::Sender<SystemEvent>)
where
    A: Actor,
{
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(actor.run(rx, event_tx));

    #[cfg(not(target_arch = "wasm32"))]
    {
        // For native testing, we don't spawn in background
        // Tests will call actor.run() directly
        // This function should not be called in native code - use actor.run() directly instead
        let _ = (actor, rx, event_tx);
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    struct TestActor {
        init_called: bool,
        messages_received: Vec<String>,
        event_tx: mpsc::Sender<SystemEvent>,
    }

    impl TestActor {
        fn new(event_tx: mpsc::Sender<SystemEvent>) -> Self {
            Self {
                init_called: false,
                messages_received: Vec::new(),
                event_tx,
            }
        }
    }

    impl Actor for TestActor {
        type Message = String;

        fn name(&self) -> &'static str {
            "TestActor"
        }

        async fn init(&mut self) -> Result<(), ActorError> {
            self.init_called = true;
            Ok(())
        }

        async fn handle(&mut self, msg: Self::Message) -> Result<(), ActorError> {
            self.messages_received.push(msg.clone());
            let _ = self.event_tx.clone().try_send(SystemEvent::StatusUpdate {
                message: format!("Received: {}", msg),
            });
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_actor_lifecycle() {
        let (mut tx, rx) = mpsc::channel(100);
        let (event_tx, event_rx) = mpsc::channel(100);

        let actor = TestActor::new(event_tx.clone());

        // Send some messages
        tx.try_send("msg1".into()).ok();
        tx.try_send("msg2".into()).ok();
        drop(tx); // Close channel to stop actor

        // Run actor
        actor.run(rx, event_tx).await;

        // Verify events sent (this proves messages were processed)
        let events: Vec<_> = event_rx.collect().await;
        assert_eq!(events.len(), 2);
        match &events[0] {
            SystemEvent::StatusUpdate { message } => {
                assert_eq!(message, "Received: msg1");
            }
            _ => panic!("Wrong event type"),
        }
        match &events[1] {
            SystemEvent::StatusUpdate { message } => {
                assert_eq!(message, "Received: msg2");
            }
            _ => panic!("Wrong event type"),
        }
    }

    #[tokio::test]
    async fn test_actor_error_handling() {
        struct FailingActor {
            _event_tx: mpsc::Sender<SystemEvent>,
        }

        impl Actor for FailingActor {
            type Message = String;

            fn name(&self) -> &'static str {
                "FailingActor"
            }

            async fn init(&mut self) -> Result<(), ActorError> {
                Err(ActorError::Other("Init failed".into()))
            }

            async fn handle(&mut self, _msg: Self::Message) -> Result<(), ActorError> {
                Ok(())
            }
        }

        let (_tx, rx) = mpsc::channel(100);
        let (event_tx, event_rx) = mpsc::channel(100);

        let actor = FailingActor {
            _event_tx: event_tx.clone(),
        };
        actor.run(rx, event_tx).await;

        // Should receive error event
        let events: Vec<_> = event_rx.collect().await;
        assert_eq!(events.len(), 1);
        match &events[0] {
            SystemEvent::Error { message } => {
                assert!(message.contains("init failed"));
            }
            _ => panic!("Wrong event type"),
        }
    }
}
