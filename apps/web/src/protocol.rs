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
            _ => panic!("Wrong variant, expected Status"),
        }
    }

    #[test]
    fn test_protocol_data_batch() {
        let frame = Frame::new_rx(vec![0x01, 0x02], 1000);
        let event = DecodedEvent::new(1001, "TEST", "DEBUG_MSG");

        let msg = WorkerToUi::DataBatch {
            frames: vec![frame],
            events: vec![event],
        };

        let json = serde_json::to_string(&msg).unwrap();
        let decoded: WorkerToUi = serde_json::from_str(&json).unwrap();

        match decoded {
            WorkerToUi::DataBatch { frames, events } => {
                assert_eq!(frames.len(), 1);
                assert_eq!(frames[0].bytes, vec![0x01, 0x02]);
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].summary, "DEBUG_MSG");
            }
            _ => panic!("Wrong variant, expected DataBatch"),
        }
    }

    #[test]
    fn test_protocol_analyze_request_result() {
        // Request
        let req = UiToWorker::AnalyzeRequest {
            baud_rate: 9600,
            data: vec![0xFE, 0x01],
        };
        let json_req = serde_json::to_string(&req).unwrap();
        let decoded_req: UiToWorker = serde_json::from_str(&json_req).unwrap();

        match decoded_req {
            UiToWorker::AnalyzeRequest { baud_rate, data } => {
                assert_eq!(baud_rate, 9600);
                assert_eq!(data, vec![0xFE, 0x01]);
            }
            _ => panic!("Wrong variant, expected AnalyzeRequest"),
        }

        // Result
        let res = WorkerToUi::AnalyzeResult {
            baud_rate: 9600,
            score: 0.95,
        };
        let json_res = serde_json::to_string(&res).unwrap();
        let decoded_res: WorkerToUi = serde_json::from_str(&json_res).unwrap();

        match decoded_res {
            WorkerToUi::AnalyzeResult { baud_rate, score } => {
                assert_eq!(baud_rate, 9600);
                assert!((score - 0.95).abs() < 0.001);
            }
            _ => panic!("Wrong variant, expected AnalyzeResult"),
        }
    }

    #[test]
    fn test_protocol_config_commands() {
        // Encapsulates configuration tests
        let msg = UiToWorker::SetFramer {
            id: "8N1".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("SetFramer"));

        // Ensure successful roundtrip
        let decoded: UiToWorker = serde_json::from_str(&json).unwrap();
        match decoded {
            UiToWorker::SetFramer { id } => assert_eq!(id, "8N1"),
            _ => panic!("Wrong variant"),
        }
    }
}
