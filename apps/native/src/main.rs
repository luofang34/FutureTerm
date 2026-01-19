use core_types::{DecodedEvent, Decoder, Frame};
use dec_nmea::NmeaDecoder;
use std::time::Instant;

const BENCH_ITERATIONS: u64 = 50_000_000;

fn main() {
    println!("Starting NMEA Decoder Benchmark...");
    println!("Iterations: {}", BENCH_ITERATIONS);

    // 1. Setup Decoder
    let mut decoder = NmeaDecoder::new();

    // 2. Prepare Data (A mix of valid and invalid frames)
    // GPGGA example
    let gpgga =
        b"$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76\r\n".to_vec();
    // GPRMC example
    let gprmc =
        b"$GPRMC,092750.000,A,5321.6802,N,00630.3372,W,0.02,31.66,280511,,,A*43\r\n".to_vec();
    // Invalid example
    let invalid = b"$NOTNMEA,123,456*00\r\n".to_vec();

    let frames = vec![
        Frame::new_rx(gpgga, 100),
        Frame::new_rx(gprmc, 200),
        Frame::new_rx(invalid, 300),
    ];

    // 3. Warmup
    let mut event = DecodedEvent::new(0, "", "");
    for _ in 0..1000 {
        for frame in &frames {
            let _ = decoder.ingest_into(frame, &mut event);
        }
    }

    // 4. Benchmark Loop
    let start = Instant::now();
    let mut count = 0;

    for _ in 0..BENCH_ITERATIONS {
        for frame in &frames {
            // "Zero-Allocation" pattern:
            // Reuse `event` buffer, avoiding malloc/free for every packet.
            if decoder.ingest_into(frame, &mut event) {
                count += 1;
            }
        }
    }

    let duration = start.elapsed();
    let total_ops = BENCH_ITERATIONS * frames.len() as u64;

    println!("Benchmark Complete.");
    println!("Total Operations: {}", total_ops);
    println!("Found Events: {}", count);
    println!("Duration: {:.2?}", duration);
    println!(
        "Throughput: {:.2} ops/sec",
        total_ops as f64 / duration.as_secs_f64()
    );
}
