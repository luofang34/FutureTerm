/// Calculates the likelihood of a buffer being 8N1 (Standard ASCII/UTF-8 Text).
/// Score is the ratio of printable characters to total characters.
pub fn calculate_score_8n1(buf: &[u8]) -> f32 {
    let mut p_count = 0;
    let mut total = 0;
    for &b in buf {
        total += 1;
        let c = b as char;
        // Check for ASCII graphic, space, CR, LF
        if c.is_ascii_graphic() || c == ' ' || c == '\r' || c == '\n' {
            p_count += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        p_count as f32 / total as f32
    }
}

/// Calculates the likelihood of a buffer being 7E1 (7 Data, Even Parity).
/// Score is the ratio of parity-valid printable characters to total characters.
pub fn calculate_score_7e1(buf: &[u8]) -> f32 {
    let mut p_count = 0;
    let mut total = 0;
    for &b in buf {
        total += 1;
        let data = b & 0x7F; // 7 data bits
        let received_parity = (b & 0x80) >> 7; // 8th bit is parity

        let ones = data.count_ones();
        // Even parity: ones + parity bit should be even.
        // So parity bit should be 0 if ones is even, 1 if ones is odd.
        let expected_parity = if ones % 2 == 0 { 0 } else { 1 };

        if received_parity == expected_parity {
            let c = data as char;
            if c.is_ascii_graphic() || c == ' ' || c == '\r' || c == '\n' {
                p_count += 1;
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        p_count as f32 / total as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_8n1_ascii_text() {
        let data = "Hello World\r\n".as_bytes();
        assert!(calculate_score_8n1(data) > 0.9);
    }

    #[test]
    fn test_8n1_empty_buffer() {
        let data: &[u8] = &[];
        assert_eq!(calculate_score_8n1(data), 0.0);
    }

    #[test]
    fn test_8n1_binary_garbage() {
        // High-byte binary data should score low
        let data: Vec<u8> = (128..255).collect();
        assert!(calculate_score_8n1(&data) < 0.5);
    }

    #[test]
    fn test_8n1_nmea_sentence() {
        let data = b"$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47\r\n";
        assert!(calculate_score_8n1(data) > 0.95);
    }

    #[test]
    fn test_8n1_mixed_content() {
        // 50% printable, 50% binary
        let mut data = Vec::new();
        data.extend_from_slice(b"Hello");
        data.extend_from_slice(&[0x80, 0x90, 0xA0, 0xB0, 0xC0]);
        let score = calculate_score_8n1(&data);
        assert!(score > 0.4 && score < 0.6);
    }

    #[test]
    fn test_7e1_valid_parity() {
        // Construct 7E1 data: 'A' (0x41 = 1000001, 2 ones -> parity 0 -> 0x41)
        // 'C' (0x43 = 1000011, 3 ones -> parity 1 -> 0xC3)
        let data = vec![0x41, 0xC3];
        assert!(calculate_score_7e1(&data) > 0.9);
        assert!(calculate_score_8n1(&data) < 0.6); // 0xC3 is not ascii graphic
    }

    #[test]
    fn test_7e1_empty_buffer() {
        let data: &[u8] = &[];
        assert_eq!(calculate_score_7e1(data), 0.0);
    }

    #[test]
    fn test_7e1_invalid_parity() {
        // 'A' with wrong parity (should be 0x41 but using 0xC1 = parity 1)
        // 0x41 has 2 ones, so even parity expects parity bit = 0
        // 0xC1 has parity bit = 1, which is wrong for even parity
        let data = vec![0xC1];
        assert!(calculate_score_7e1(&data) < 0.5);
    }

    #[test]
    fn test_score_comparison_wrong_baud() {
        // Simulated wrong baud rate produces garbage
        // At wrong baud, we get framing errors resulting in non-printable bytes
        let garbage: Vec<u8> = vec![0xFF, 0xFE, 0x7F, 0x80, 0x00, 0x01, 0xFF, 0x82];
        assert!(calculate_score_8n1(&garbage) < 0.3);
        assert!(calculate_score_7e1(&garbage) < 0.3);
    }
}
