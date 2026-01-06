use clap::Parser;
use std::io::{self, Write};
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
   /// Serial port to open
   #[arg(default_value = "/dev/cu.usbserial-A5069RR4")]
   port: String,

   /// Baud rate
   #[arg(short, long, default_value_t = 1500_000)]
   baud: u32,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    
    println!("Opening {} at {} baud...", args.port, args.baud);

    let mut port = serialport::new(&args.port, args.baud)
        .timeout(Duration::from_millis(10))
        .open()?;

    let mut serial_buf: Vec<u8> = vec![0; 1000];
    println!("Connected. Press Ctrl+C to exit.");
    
    loop {
        match port.read(serial_buf.as_mut_slice()) {
            Ok(t) => {
                io::stdout().write_all(&serial_buf[..t])?;
                io::stdout().flush()?;
            },
            Err(ref e) if e.kind() == io::ErrorKind::TimedOut => (),
            Err(e) => eprintln!("{:?}", e),
        }
    }
}
