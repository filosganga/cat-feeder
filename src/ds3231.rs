//! The DS3231's registers: bytes in, a time and its health out.
//!
//! Pure logic. `rtc.rs` moves the bytes over I²C; this decides what they say,
//! host-tested, because a clock that misreads its own BCD is a feeder that
//! feeds at the wrong hour with nothing on the console to say why.
//!
//! ## What is read
//!
//! One burst from register `0x00` to `0x12`, nineteen bytes:
//!
//! | Register | Holds |
//! |---|---|
//! | `0x00`–`0x06` | seconds, minutes, hours, weekday, date, month, year — BCD |
//! | `0x0E` | control. Bit 7, `EOSC`, set means the oscillator **stops on battery** |
//! | `0x0F` | status. Bit 7, `OSF`, set means the oscillator **has stopped** at some point |
//! | `0x11`–`0x12` | temperature, °C, in quarter degrees |
//!
//! ## The flag is the reason for the part
//!
//! `OSF` is set at first power-up and whenever the oscillator loses power —
//! a flat or missing coin cell. So "this clock was never set, or has lost its
//! time" is a fact the firmware reads rather than infers. A cleared RTC
//! otherwise reads as a plausible date, and a plausible date is exactly what
//! *never guess* exists to refuse. See *Which means an RTC* in `CLAUDE.md`.
//!
//! `EOSC` is the other half: with it set, the chip keeps time on mains and
//! silently stops on the coin cell, so a power cut sets `OSF` every time. It is
//! clear at first power-up and this module clears it on every write, but a
//! module that arrives with it set would look exactly like a dead battery.
//!
//! ## Local time, no century
//!
//! The chip holds **local wall-clock time**, the same frame `feeder/time` and
//! the schedule slots use, so nothing converts. The year register is two
//! digits and the century bit is ignored and written clear: 2000–2099.

use crate::schedule::{Date, Wall, seconds_between};

/// The DS3231's I²C address. Fixed, unlike the panel's.
pub const ADDRESS: u8 = 0x68;

/// Registers `0x00..=0x12`.
pub const REGISTERS: usize = 0x13;

/// Where the time starts, and the status register.
pub const TIME_REG: u8 = 0x00;
pub const CONTROL_REG: u8 = 0x0E;
pub const STATUS_REG: u8 = 0x0F;

const OSF: u8 = 0x80;
const EOSC: u8 = 0x80;

/// What the chip says, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// The time held, or `None` if the registers do not form a real date —
    /// which is itself worth seeing, and is never to be believed.
    pub wall: Option<Wall>,
    /// `OSF`: the oscillator has stopped since the flag was last cleared. The
    /// time is not to be trusted, however plausible it looks.
    pub stopped: bool,
    /// `EOSC`: the oscillator is configured to stop on battery.
    pub stops_on_battery: bool,
    /// Die temperature in quarter degrees Celsius. A reading that changes and
    /// is roughly room temperature is the cheapest proof the part is alive.
    pub temperature_q: i16,
}

impl Reading {
    /// Whether the time can be used at all: set, never stopped, and a date.
    pub fn trustworthy(&self) -> Option<Wall> {
        if self.stopped { None } else { self.wall }
    }
}

/// How far the chip may be from a trusted time before it is rewritten.
///
/// Two seconds: `feeder/time` is whole seconds and lands a few hundred
/// milliseconds after it was stamped, so one second of disagreement is noise.
/// The chip is good to ±2 ppm — about a minute a year — so in practice it is
/// written once and then left alone.
pub const TOLERANCE_S: i64 = 2;

/// Whether the chip should be rewritten to `now`: it cannot be trusted, holds
/// no date at all, or is more than [`TOLERANCE_S`] out — in either direction,
/// and across any span of dates.
pub fn needs_set(reading: &Reading, now: Wall) -> bool {
    match reading.trustworthy() {
        None => true,
        Some(held) => seconds_between(now, held).abs() > TOLERANCE_S,
    }
}

/// Decodes a burst read of registers `0x00..=0x12`.
pub fn decode(regs: &[u8; REGISTERS]) -> Reading {
    Reading {
        wall: decode_time(&regs[0..7]),
        stopped: regs[STATUS_REG as usize] & OSF != 0,
        stops_on_battery: regs[CONTROL_REG as usize] & EOSC != 0,
        temperature_q: ((regs[0x11] as i8 as i16) << 2) | (regs[0x12] >> 6) as i16,
    }
}

fn decode_time(t: &[u8]) -> Option<Wall> {
    let second = bcd(t[0] & 0x7F)?;
    let minute = bcd(t[1] & 0x7F)?;
    let hour = decode_hour(t[2])?;
    let day = bcd(t[4] & 0x3F)?;
    let month = bcd(t[5] & 0x1F)?;
    let year = bcd(t[6])?;

    if second > 59 || minute > 59 || hour > 23 {
        return None;
    }

    let date = Date {
        year: 2000 + year as u16,
        month,
        day,
    };
    if !date.is_valid() {
        return None;
    }

    Some(Wall {
        date,
        second_of_day: hour as u32 * 3600 + minute as u32 * 60 + second as u32,
        // The chip holds wall time and nothing else. `Wall`'s offset is
        // informational only, and this source has none to report.
        offset_minutes: None,
    })
}

/// Bit 6 selects 12-hour mode. This module always writes 24-hour, but a module
/// set by something else may arrive in 12-hour mode, and misreading it would
/// put every meal twelve hours out.
fn decode_hour(raw: u8) -> Option<u8> {
    if raw & 0x40 == 0 {
        return bcd(raw & 0x3F);
    }
    let hour12 = bcd(raw & 0x1F)?;
    if !(1..=12).contains(&hour12) {
        return None;
    }
    let pm = raw & 0x20 != 0;
    Some(match (hour12, pm) {
        (12, false) => 0,
        (12, true) => 12,
        (h, false) => h,
        (h, true) => h + 12,
    })
}

/// The seven time registers for `wall`, from `0x00`. `None` for a year the
/// chip cannot hold.
pub fn encode_time(wall: Wall) -> Option<[u8; 7]> {
    let year = wall.date.year.checked_sub(2000).filter(|y| *y < 100)? as u8;
    let s = wall.second_of_day % 86_400;
    Some([
        to_bcd((s % 60) as u8),
        to_bcd((s / 60 % 60) as u8),
        // 24-hour mode: bit 6 clear.
        to_bcd((s / 3600) as u8),
        weekday(wall.date),
        to_bcd(wall.date.day),
        // Century bit clear.
        to_bcd(wall.date.month),
        to_bcd(year),
    ])
}

/// The status byte to write after setting the time: `OSF` cleared, everything
/// else as it was.
pub fn cleared_status(status: u8) -> u8 {
    status & !OSF
}

/// The control byte to write after setting the time: `EOSC` cleared, so the
/// oscillator keeps running on the coin cell.
pub fn running_control(control: u8) -> u8 {
    control & !EOSC
}

/// 1 = Monday … 7 = Sunday. The chip only increments this register and never
/// checks it against the date, so it is written for tidiness, not used.
fn weekday(date: Date) -> u8 {
    // Sakamoto's method, giving 0 = Sunday.
    const T: [u16; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if date.month < 3 {
        date.year - 1
    } else {
        date.year
    };
    let dow = (y + y / 4 - y / 100 + y / 400 + T[(date.month - 1) as usize] + date.day as u16) % 7;
    if dow == 0 { 7 } else { dow as u8 }
}

fn bcd(byte: u8) -> Option<u8> {
    let (hi, lo) = (byte >> 4, byte & 0x0F);
    (hi <= 9 && lo <= 9).then_some(hi * 10 + lo)
}

fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(y: u16, mo: u8, d: u8, h: u32, mi: u32, s: u32) -> Wall {
        Wall {
            date: Date {
                year: y,
                month: mo,
                day: d,
            },
            second_of_day: h * 3600 + mi * 60 + s,
            offset_minutes: None,
        }
    }

    /// A full register image with `time` in `0x00..0x07` and the rest given.
    fn regs(time: [u8; 7], control: u8, status: u8, temp: (u8, u8)) -> [u8; REGISTERS] {
        let mut r = [0u8; REGISTERS];
        r[..7].copy_from_slice(&time);
        r[CONTROL_REG as usize] = control;
        r[STATUS_REG as usize] = status;
        r[0x11] = temp.0;
        r[0x12] = temp.1;
        r
    }

    #[test]
    fn a_set_clock_decodes_to_local_wall_time() {
        // 2026-09-25 19:22:06, a Friday.
        let r = decode(&regs(
            [0x06, 0x22, 0x19, 5, 0x25, 0x09, 0x26],
            0x1C,
            0x08,
            (25, 0x40),
        ));

        assert_eq!(r.wall, Some(wall(2026, 9, 25, 19, 22, 6)));
        assert!(!r.stopped);
        assert!(!r.stops_on_battery);
        assert_eq!(r.temperature_q, 25 * 4 + 1, "25.25 °C");
        assert_eq!(r.trustworthy(), r.wall);
    }

    /// What a fresh or battery-less module reads: a real-looking date, and the
    /// flag that says not to believe it.
    #[test]
    fn a_stopped_oscillator_is_never_trustworthy() {
        let r = decode(&regs(
            [0x12, 0x03, 0x00, 1, 0x01, 0x01, 0x00],
            0x1C,
            0x88,
            (24, 0),
        ));

        assert_eq!(r.wall, Some(wall(2000, 1, 1, 0, 3, 12)));
        assert!(r.stopped);
        assert_eq!(r.trustworthy(), None);
    }

    #[test]
    fn eosc_is_reported() {
        let r = decode(&regs([0, 0, 0, 1, 1, 1, 0], 0x9C, 0, (0, 0)));
        assert!(r.stops_on_battery);
    }

    #[test]
    fn twelve_hour_mode_is_read_correctly() {
        let at = |hour_reg: u8| {
            decode(&regs([0, 0, hour_reg, 1, 0x01, 0x01, 0x26], 0, 0, (0, 0)))
                .wall
                .map(|w| w.second_of_day / 3600)
        };

        assert_eq!(at(0x40 | 0x12), Some(0), "12 AM is midnight");
        assert_eq!(at(0x40 | 0x01), Some(1));
        assert_eq!(at(0x40 | 0x20 | 0x12), Some(12), "12 PM is noon");
        assert_eq!(at(0x40 | 0x20 | 0x07), Some(19));
        assert_eq!(at(0x40 | 0x13), None, "13 is not a 12-hour hour");
    }

    #[test]
    fn garbage_is_not_a_time() {
        for time in [
            [0x6A, 0, 0, 1, 1, 1, 0],       // not BCD
            [0, 0x60, 0, 1, 1, 1, 0],       // minute 60
            [0, 0, 0x24, 1, 1, 1, 0],       // hour 24
            [0, 0, 0, 1, 0x31, 0x02, 0x26], // 31 February
            [0, 0, 0, 1, 0x00, 0x01, 0x26], // day 0
            [0, 0, 0, 1, 0x01, 0x13, 0x26], // month 13
            [0xFF; 7],                      // a bus reading nothing
        ] {
            assert_eq!(decode(&regs(time, 0, 0, (0, 0))).wall, None, "{time:02x?}");
        }
    }

    #[test]
    fn a_leap_day_is_a_date() {
        let r = decode(&regs([0, 0, 0, 4, 0x29, 0x02, 0x28], 0, 0, (0, 0)));
        assert_eq!(r.wall, Some(wall(2028, 2, 29, 0, 0, 0)));
    }

    #[test]
    fn negative_temperatures_keep_their_sign() {
        // -1.75 °C: MSB -2, fraction 0.25 → -2 + 0.25 = -1.75, i.e. -7 quarters.
        let r = decode(&regs([0, 0, 0, 1, 1, 1, 0], 0, 0, (0xFE, 0x40)));
        assert_eq!(r.temperature_q, -7);
    }

    #[test]
    fn encoding_round_trips_through_decoding() {
        for w in [
            wall(2026, 9, 25, 19, 22, 6),
            wall(2000, 1, 1, 0, 0, 0),
            wall(2099, 12, 31, 23, 59, 59),
            wall(2028, 2, 29, 12, 0, 0),
        ] {
            let mut r = [0u8; REGISTERS];
            r[..7].copy_from_slice(&encode_time(w).unwrap());
            assert_eq!(decode(&r).wall, Some(w));
        }
    }

    #[test]
    fn a_year_the_chip_cannot_hold_is_refused() {
        assert_eq!(encode_time(wall(1999, 12, 31, 0, 0, 0)), None);
        assert_eq!(encode_time(wall(2100, 1, 1, 0, 0, 0)), None);
    }

    /// Writes are always 24-hour with the century bit clear, whatever the
    /// module held before.
    #[test]
    fn encoding_is_24_hour_without_century() {
        let t = encode_time(wall(2026, 9, 25, 19, 22, 6)).unwrap();
        assert_eq!(t[2] & 0x40, 0);
        assert_eq!(t[5] & 0x80, 0);
        assert_eq!(t, [0x06, 0x22, 0x19, 5, 0x25, 0x09, 0x26]);
    }

    #[test]
    fn weekdays_are_monday_first() {
        assert_eq!(
            weekday(Date {
                year: 2026,
                month: 9,
                day: 25
            }),
            5,
            "Friday"
        );
        assert_eq!(
            weekday(Date {
                year: 2026,
                month: 9,
                day: 27
            }),
            7,
            "Sunday"
        );
        assert_eq!(
            weekday(Date {
                year: 2000,
                month: 1,
                day: 1
            }),
            6,
            "Saturday"
        );
        assert_eq!(
            weekday(Date {
                year: 2028,
                month: 2,
                day: 29
            }),
            2,
            "Tuesday"
        );
    }

    fn reading(held: Option<Wall>, stopped: bool) -> Reading {
        Reading {
            wall: held,
            stopped,
            stops_on_battery: false,
            temperature_q: 100,
        }
    }

    #[test]
    fn a_chip_within_tolerance_is_left_alone() {
        let now = wall(2026, 9, 25, 20, 0, 0);
        for held in [
            wall(2026, 9, 25, 20, 0, 0),
            wall(2026, 9, 25, 20, 0, 2),
            wall(2026, 9, 25, 19, 59, 58),
        ] {
            assert!(!needs_set(&reading(Some(held), false), now), "{held}");
        }
    }

    #[test]
    fn a_chip_out_by_more_is_rewritten_either_way() {
        let now = wall(2026, 9, 25, 20, 0, 0);
        assert!(needs_set(
            &reading(Some(wall(2026, 9, 25, 20, 0, 3)), false),
            now
        ));
        assert!(needs_set(
            &reading(Some(wall(2026, 9, 25, 19, 59, 57)), false),
            now
        ));
    }

    /// Across midnight is two seconds, not a day.
    #[test]
    fn midnight_is_not_a_difference_of_a_day() {
        let now = wall(2026, 9, 26, 0, 0, 1);
        assert!(!needs_set(
            &reading(Some(wall(2026, 9, 25, 23, 59, 59)), false),
            now
        ));
    }

    /// The case the old one-day arithmetic got wrong: a year out, at almost the
    /// same time of day, came out as two seconds and was left alone.
    #[test]
    fn a_chip_a_year_out_is_rewritten() {
        let now = wall(2026, 9, 26, 0, 0, 1);
        assert!(needs_set(
            &reading(Some(wall(2025, 9, 25, 23, 59, 59)), false),
            now
        ));
    }

    #[test]
    fn a_stopped_or_dateless_chip_is_always_rewritten() {
        let now = wall(2026, 9, 25, 20, 0, 0);
        assert!(
            needs_set(&reading(Some(now), true), now),
            "OSF set, even on time"
        );
        assert!(
            needs_set(&reading(None, false), now),
            "no date in the registers"
        );
    }

    #[test]
    fn setting_clears_the_flags_and_nothing_else() {
        assert_eq!(cleared_status(0x88), 0x08);
        assert_eq!(cleared_status(0x08), 0x08);
        assert_eq!(running_control(0x9C), 0x1C);
        assert_eq!(running_control(0x1C), 0x1C);
    }
}
