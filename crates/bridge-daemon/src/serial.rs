use crate::protocol::{PortInfo, PortType};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex};
use tokio_serial::{SerialPort, SerialPortBuilderExt, SerialStream};

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

    /// Open a serial port and start reading
    pub fn open(
        &mut self,
        path: &str,
        baud_rate: u32,
        data_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Result<(), String> {
        // Close existing port if open
        self.close();

        // Open the serial port
        let port = tokio_serial::new(path, baud_rate)
            .open_native_async()
            .map_err(|e| format!("Failed to open port {}: {}", path, e))?;

        let port_arc = Arc::new(Mutex::new(port));
        let read_port = port_arc.clone();

        // Spawn read task
        let read_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buffer = vec![0u8; 1024];
            loop {
                let result = {
                    let mut port = read_port.lock().await;
                    port.read(&mut buffer).await
                };

                match result {
                    Ok(0) => {
                        eprintln!("Serial port closed (EOF)");
                        break;
                    }
                    Ok(n) => {
                        let data = match buffer.get(..n) {
                            Some(slice) => slice.to_vec(),
                            None => {
                                eprintln!("Buffer slice error: invalid range 0..{}", n);
                                break;
                            }
                        };
                        if data_tx.send(data).is_err() {
                            eprintln!("Data channel closed, stopping read task");
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Serial read error: {}", e);
                        break;
                    }
                }
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

    /// Close the serial port
    pub fn close(&mut self) {
        // Abort read task
        if let Some(task) = self.read_task.take() {
            task.abort();
        }

        // Drop the port (closes it)
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
        self.close();
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
