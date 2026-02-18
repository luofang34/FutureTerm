use crate::protocol::{PortInfo, PortType};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex};
use tokio_serial::{SerialPort, SerialPortBuilderExt, SerialStream};

/// Timeout for each serial read iteration (milliseconds).
/// Kept short so that `set_config()` / `write()` can acquire the port mutex
/// promptly during baud-rate probing. A long timeout here would block config
/// changes for its entire duration because the read task holds the mutex.
const READ_TIMEOUT_MS: u64 = 100;

/// How many read-timeout iterations between device-path existence checks.
/// 20 × 100ms = every ~2 seconds.
const PATH_CHECK_INTERVAL: u64 = 20;

/// Serial port manager
pub struct SerialManager {
    port: Option<Arc<Mutex<SerialStream>>>,
    read_task: Option<tokio::task::JoinHandle<()>>,
}

impl SerialManager {
    pub fn new() -> Self {
        Self {
            port: None,
            read_task: None,
        }
    }

    /// List available serial ports
    pub fn list_ports() -> Result<Vec<PortInfo>, String> {
        let ports =
            tokio_serial::available_ports().map_err(|e| format!("Failed to list ports: {}", e))?;

        Ok(ports
            .into_iter()
            .map(|p| {
                let (port_type, vid, pid, serial_number, manufacturer, product) = match &p.port_type
                {
                    tokio_serial::SerialPortType::UsbPort(info) => (
                        PortType::UsbSerial,
                        Some(info.vid),
                        Some(info.pid),
                        info.serial_number.clone(),
                        info.manufacturer.clone(),
                        info.product.clone(),
                    ),
                    tokio_serial::SerialPortType::BluetoothPort => {
                        (PortType::Bluetooth, None, None, None, None, None)
                    }
                    tokio_serial::SerialPortType::PciPort => {
                        (PortType::Pci, None, None, None, None, None)
                    }
                    tokio_serial::SerialPortType::Unknown => {
                        (PortType::Unknown, None, None, None, None, None)
                    }
                };

                PortInfo {
                    path: p.port_name,
                    port_type,
                    vid,
                    pid,
                    serial_number,
                    manufacturer,
                    product,
                }
            })
            .collect())
    }

    /// Open a serial port and start reading.
    ///
    /// `disconnect_tx` fires when the serial port disconnects (device unplugged / read error).
    pub async fn open(
        &mut self,
        path: &str,
        baud_rate: u32,
        data_tx: mpsc::UnboundedSender<Vec<u8>>,
        disconnect_tx: Option<tokio::sync::oneshot::Sender<String>>,
    ) -> Result<(), String> {
        // Close existing port if open (must await to ensure fd is released)
        self.close().await;

        // Open the serial port with retry for "Device or resource busy".
        // macOS FTDI drivers can take extra time to release the device node
        // after close(fd), so we retry a few times with small delays.
        let mut port = None;
        let mut last_err = String::new();
        for attempt in 0..4 {
            match tokio_serial::new(path, baud_rate).open_native_async() {
                Ok(mut p) => {
                    // Explicitly disable hardware flow control and assert control signals.
                    // macOS FTDI driver (AppleUSBFTDI) may default to CRTSCTS on.
                    // With CRTSCTS on and CTS not asserted (common in 3-wire setups),
                    // writes succeed in the kernel buffer but the FT232 never transmits.
                    if let Err(e) = p.set_flow_control(tokio_serial::FlowControl::None) {
                        eprintln!("Warning: set_flow_control failed: {}", e);
                    }
                    // Assert DTR - signals host is ready. Some devices gate TX on DTR.
                    if let Err(e) = p.write_data_terminal_ready(true) {
                        eprintln!("Warning: set DTR failed: {}", e);
                    }
                    // Assert RTS - in some wiring configurations RTS is looped to CTS.
                    if let Err(e) = p.write_request_to_send(true) {
                        eprintln!("Warning: set RTS failed: {}", e);
                    }
                    eprintln!(
                        "Port {} opened at {} baud (flow=None, DTR=1, RTS=1)",
                        path, baud_rate
                    );
                    port = Some(p);
                    break;
                }
                Err(e) => {
                    last_err = format!("Failed to open port {}: {}", path, e);
                    let err_str = e.to_string().to_lowercase();
                    if err_str.contains("busy") || err_str.contains("resource") {
                        eprintln!(
                            "Port busy, retry {}/3 after 150ms: {}",
                            attempt + 1,
                            last_err
                        );
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        continue;
                    }
                    return Err(last_err);
                }
            }
        }
        let port = port.ok_or(last_err)?;

        let port_arc = Arc::new(Mutex::new(port));
        let read_port = port_arc.clone();
        let path_owned = path.to_string();

        // Spawn read task with device-existence watchdog.
        // Some macOS USB-serial drivers hang on read() when the device is unplugged
        // instead of returning an error. We use tokio::time::timeout to periodically
        // break out and check if the device path still exists.
        let read_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buffer = vec![0u8; 1024];
            #[allow(unused_assignments)]
            let mut reason = String::new();
            let mut idle_ticks: u64 = 0;
            loop {
                // Short timeout so the mutex is released frequently, allowing
                // set_config / write to acquire it without multi-second waits.
                let result = tokio::time::timeout(Duration::from_millis(READ_TIMEOUT_MS), async {
                    let mut port = read_port.lock().await;
                    port.read(&mut buffer).await
                })
                .await;

                match result {
                    Ok(Ok(0)) => {
                        eprintln!("Serial port closed (EOF)");
                        reason = "EOF".into();
                        break;
                    }
                    Ok(Ok(n)) => {
                        idle_ticks = 0;
                        let data = match buffer.get(..n) {
                            Some(slice) => slice.to_vec(),
                            None => {
                                eprintln!("Buffer slice error: invalid range 0..{}", n);
                                reason = "Buffer error".into();
                                break;
                            }
                        };
                        if data_tx.send(data).is_err() {
                            eprintln!("Data channel closed, stopping read task");
                            reason = "Channel closed".into();
                            break;
                        }
                    }
                    Ok(Err(e)) => {
                        eprintln!("Serial read error: {}", e);
                        reason = format!("Read error: {}", e);
                        break;
                    }
                    Err(_) => {
                        // Read timed out (no data within READ_TIMEOUT_MS).
                        // Periodically check if device path still exists.
                        idle_ticks += 1;
                        if idle_ticks.is_multiple_of(PATH_CHECK_INTERVAL)
                            && !std::path::Path::new(&path_owned).exists()
                        {
                            eprintln!(
                                "Device path {} disappeared, device was unplugged",
                                path_owned
                            );
                            reason = "Device removed".into();
                            break;
                        }
                        // Mutex released here, allowing set_config/write to proceed
                    }
                }
            }

            // Notify that the serial port disconnected
            if let Some(tx) = disconnect_tx {
                let _ = tx.send(reason);
            }
        });

        self.port = Some(port_arc);
        self.read_task = Some(read_task);

        Ok(())
    }

    /// Write data to the serial port
    pub async fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        let port = self.port.as_ref().ok_or("No port open".to_string())?;

        let mut port_guard = port.lock().await;
        port_guard
            .write_all(data)
            .await
            .map_err(|e| format!("Failed to write: {}", e))?;

        Ok(data.len())
    }

    /// Set serial port configuration
    pub async fn set_config(
        &mut self,
        baud_rate: Option<u32>,
        data_bits: Option<u8>,
        stop_bits: Option<u8>,
        parity: Option<String>,
    ) -> Result<(), String> {
        let port = self.port.as_ref().ok_or("No port open".to_string())?;

        let mut port_guard = port.lock().await;

        if let Some(baud) = baud_rate {
            port_guard
                .set_baud_rate(baud)
                .map_err(|e| format!("Failed to set baud rate: {}", e))?;
        }

        if let Some(bits) = data_bits {
            let data_bits = match bits {
                5 => tokio_serial::DataBits::Five,
                6 => tokio_serial::DataBits::Six,
                7 => tokio_serial::DataBits::Seven,
                8 => tokio_serial::DataBits::Eight,
                _ => return Err(format!("Invalid data bits: {}", bits)),
            };
            port_guard
                .set_data_bits(data_bits)
                .map_err(|e| format!("Failed to set data bits: {}", e))?;
        }

        if let Some(bits) = stop_bits {
            let stop_bits = match bits {
                1 => tokio_serial::StopBits::One,
                2 => tokio_serial::StopBits::Two,
                _ => return Err(format!("Invalid stop bits: {}", bits)),
            };
            port_guard
                .set_stop_bits(stop_bits)
                .map_err(|e| format!("Failed to set stop bits: {}", e))?;
        }

        if let Some(p) = parity {
            let parity = match p.to_lowercase().as_str() {
                "none" => tokio_serial::Parity::None,
                "odd" => tokio_serial::Parity::Odd,
                "even" => tokio_serial::Parity::Even,
                _ => return Err(format!("Invalid parity: {}", p)),
            };
            port_guard
                .set_parity(parity)
                .map_err(|e| format!("Failed to set parity: {}", e))?;
        }

        Ok(())
    }

    /// Close the serial port, waiting for the read task to fully terminate.
    /// This ensures the OS file descriptor is released before returning,
    /// preventing "Device or resource busy" errors on reopen.
    pub async fn close(&mut self) {
        // Abort read task and wait for it to actually finish.
        // abort() is non-blocking - we must await the handle to ensure
        // the task drops its Arc<Mutex<SerialStream>> clone.
        if let Some(task) = self.read_task.take() {
            task.abort();
            let _ = task.await; // JoinError from abort is expected
        }

        // Now safe to drop the port - no other Arc references exist
        self.port.take();
    }

    /// Check if a port is currently open
    #[allow(dead_code)] // May be used in future
    pub fn is_open(&self) -> bool {
        self.port.is_some()
    }
}

impl Drop for SerialManager {
    fn drop(&mut self) {
        // Best-effort cleanup (Drop can't be async).
        // The task is aborted but not awaited - acceptable since the
        // SerialManager is being destroyed (end of WebSocket session).
        if let Some(task) = self.read_task.take() {
            task.abort();
        }
        self.port.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_ports() {
        // This test may fail in CI if no serial ports are available
        let result = SerialManager::list_ports();
        assert!(result.is_ok());
    }

    #[test]
    fn test_new_manager() {
        let manager = SerialManager::new();
        assert!(!manager.is_open());
    }

    #[tokio::test]
    async fn test_write_without_open() {
        let mut manager = SerialManager::new();
        let result = manager.write(b"test").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No port open"));
    }

    #[tokio::test]
    async fn test_set_config_without_open() {
        let mut manager = SerialManager::new();
        let result = manager.set_config(Some(115200), None, None, None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No port open"));
    }
}
