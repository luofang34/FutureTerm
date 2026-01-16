use serde::{Deserialize, Serialize};
use core_types::{Frame, DecodedEvent};

#[derive(Debug, Serialize, Deserialize)]
pub enum UiToWorker {
    Connect { baud_rate: u32 },
    Disconnect,
    Send { data: Vec<u8> },
    SetSignals { dtr: bool, rts: bool },
    // Configs
    SetFramer { id: String },
    SetDecoder { id: String },
    // Simulation
    Simulate { duration_ms: u32 },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum WorkerToUi {
    Status(String), // "Connected", "Disconnected", "Error: ..."
    DataBatch {
        frames: Vec<Frame>,
        events: Vec<DecodedEvent>,
    },
}
