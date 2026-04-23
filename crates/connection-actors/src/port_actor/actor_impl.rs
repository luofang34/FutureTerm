use actor_protocol::ActorError;
use actor_runtime::{Actor, PortMessage};

use super::PortActor;

impl Actor for PortActor {
    type Message = PortMessage;

    fn name(&self) -> &'static str {
        "PortActor"
    }

    async fn handle(&mut self, msg: PortMessage) -> Result<(), ActorError> {
        match msg {
            #[cfg(target_arch = "wasm32")]
            PortMessage::Open {
                port,
                baud,
                framing,
                send_wakeup,
                operation_id,
                port_handle,
            } => {
                self.handle_open(port, baud, framing, send_wakeup, operation_id, port_handle)
                    .await?
            }

            #[cfg(not(target_arch = "wasm32"))]
            PortMessage::Open {
                port,
                baud,
                framing,
                send_wakeup,
                operation_id,
            } => {
                self.handle_open(port, baud, framing, send_wakeup, operation_id)
                    .await?
            }

            PortMessage::Close => self.handle_close().await?,
            PortMessage::Write { data } => self.handle_write(data).await?,
            PortMessage::InjectData { data } => self.handle_inject_data(data).await?,
        }

        Ok(())
    }

    async fn shutdown(&mut self) {
        // Close port on shutdown
        let _ = self.handle_close().await;
    }
}
