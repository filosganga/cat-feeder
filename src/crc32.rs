//! The checksum every record in flash carries, so erased flash and an
//! interrupted write read as nothing stored rather than as garbage.
//!
//! Pure logic, and core (ADR-0028): the credentials, the schedule, the pause
//! and the timezone records all use it.

/// CRC-32/ISO-HDLC, computed a bit at a time.
///
/// No lookup table: this runs twice per boot at most, and 1 KB of table is
/// worth more than the microseconds it would save.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The standard check value for CRC-32/ISO-HDLC, so a change here cannot
    /// silently orphan every record already written.
    #[test]
    fn the_check_value_is_the_standard_one() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }
}
