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
    if total == 0 { 0.0 } else { p_count as f32 / total as f32 }
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
    if total == 0 { 0.0 } else { p_count as f32 / total as f32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_8n1() {
        let data = "Hello World\r\n".as_bytes();
        assert!(calculate_score_8n1(data) > 0.9);
    }
    
    #[test]
    fn test_7e1() {
        // Construct 7E1 data: 'A' (0x41 = 1000001, 2 ones -> parity 0 -> 0x41)
        // 'C' (0x43 = 1000011, 3 ones -> parity 1 -> 0xC3)
        let data = vec![0x41, 0xC3]; 
        assert!(calculate_score_7e1(&data) > 0.9);
        assert!(calculate_score_8n1(&data) < 0.6); // 0xC3 is not ascii graphic
    }
}
