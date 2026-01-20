use core_types::{DecodedEvent, Frame};
use serde::{Deserialize, Serialize};

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
    StartHeartbeat,
    StopHeartbeat,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum WorkerToUi {
    Status(String), // "Connected", "Disconnected", "Error: ..."
    DataBatch {
        frames: Vec<Frame>,
        events: Vec<DecodedEvent>,
    },
    AnalyzeResult {
        baud_rate: u32,
        score: f32,
    },
    TxData {
        data: Vec<u8>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ui_to_worker_serialization() {
        let msg = UiToWorker::Connect { baud_rate: 115200 };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: UiToWorker = serde_json::from_str(&json).unwrap();
        match decoded {
            UiToWorker::Connect { baud_rate } => assert_eq!(baud_rate, 115200),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_worker_to_ui_serialization() {
        let msg = WorkerToUi::Status("Connected".to_string());
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: WorkerToUi = serde_json::from_str(&json).unwrap();
        match decoded {
            WorkerToUi::Status(s) => assert_eq!(s, "Connected"),
            _ => panic!("Wrong variant"),
        }
    }
}
