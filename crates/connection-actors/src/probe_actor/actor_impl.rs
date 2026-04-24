use actor_protocol::ActorError;
use actor_runtime::{Actor, ProbeMessage};

impl Actor for super::ProbeActor {
    type Message = ProbeMessage;

    fn name(&self) -> &'static str {
        "ProbeActor"
    }

    async fn handle(&mut self, msg: ProbeMessage) -> Result<(), ActorError> {
        match msg {
            #[cfg(target_arch = "wasm32")]
            ProbeMessage::Start { port, port_handle } => {
                self.handle_start(port, port_handle).await?
            }

            #[cfg(not(target_arch = "wasm32"))]
            ProbeMessage::Start { port } => self.handle_start(port).await?,

            ProbeMessage::Abort => self.handle_abort().await?,
        }

        Ok(())
    }
}
