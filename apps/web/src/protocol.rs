use serde::{Deserialize, Serialize};
use core_types::{Frame, DecodedEvent};

#[derive(Debug, Serialize, Deserialize)]
pub enum UiToWorker {
    Connect { baud_rate: u32 },
    Disconnect,
    Send { data: Vec<u8> },
    IngestData { data: Vec<u8>, timestamp_us: u64 },
    SetSignals { dtr: bool, rts: bool },
    // Configs
    SetFramer { id: String },
    SetDecoder { id: String },
    // Simulation
    Simulate { duration_ms: u32 },
    // Phase 3: Auto-Baud
    AnalyzeRequest { baud_rate: u32, data: Vec<u8> },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum WorkerToUi {
    Status(String), // "Connected", "Disconnected", "Error: ..."
    DataBatch {
        frames: Vec<Frame>,
        events: Vec<DecodedEvent>,
    },
    AnalyzeResult { baud_rate: u32, score: f32 },
}
