//! The clock and the feeding schedule.
//!
//! Pure logic: no broker, no timer, no executor. Wall-clock time arrives as a
//! parsed [`Wall`] and monotonic time as milliseconds, so every decision is a
//! function of its inputs and the whole thing is testable on the host.
//!
//! The unit owns both. The time comes from a DS3231 at boot if it kept time,
//! from Home Assistant's `feeder/time` every minute, or from the knob, and a
//! [`LocalClock`] advances between them — see [`TimeSource`] for how far each
//! is trusted. The schedule is kept in flash (the record is [`Schedule::encode`])
//! and replaced only by an explicit command. No NTP. A unit whose RTC lost its
//! time waits to be told rather than guessing; a unit never given a schedule
//! never feeds.
//!
//! ## Never feed twice
//!
//! The one rule worth stating plainly: **a missed meal is preferable to a
//! double one.** Three separate mechanisms enforce it, each covering a failure
//! the others cannot see.
//!
//! 1. **The consumed marker.** [`Scheduler`] remembers the day and the
//!    time-of-day of the slot it last resolved. A slot at or before that is
//!    never revisited, whether it was fed, skipped for being late, or skipped
//!    because the unit was paused.
//! 2. **The baseline pass.** The first look at the clock after boot never
//!    feeds; it only records where the day already is. Without it, rebooting a
//!    few seconds after 08:00 would dispense the 08:00 slot again, because the
//!    consumed marker lives in RAM and does not survive the reboot.
//! 3. **The lateness limit.** A slot resolved more than [`MAX_LATENESS_S`]
//!    after its time is marked consumed without feeding. This is what stops a
//!    clock correction — a `time` message that jumps the unit forward past a
//!    slot — from being mistaken for the slot falling due.
//!
//! The marker is keyed on the slot's **time of day, not its index**. Home
//! Assistant can republish a schedule with a slot inserted or removed at any
//! time, and an index would then silently point at a different meal.

use heapless::Vec;

/// A schedule longer than this is rejected rather than truncated. Eight meals a
/// day is already well past anything a cat feeder needs.
pub const MAX_SLOTS: usize = 8;

/// A slot resolved more than this long after its time is marked consumed
/// instead of being fed.
///
/// Home Assistant republishes the time every minute and the local clock runs
/// continuously between those messages, so a slot falling due normally is
/// noticed within the scheduler's tick — a second or so. Two minutes is
/// therefore a wide margin for a genuine crossing while still being far too
/// short to swallow a clock jump, which moves the unit by minutes or hours.
pub const MAX_LATENESS_S: u32 = 120;

/// A calendar date. Only ever used as an identity for "the same day".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl Date {
    fn is_leap(year: u16) -> bool {
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    }

    pub(crate) fn days_in_month(year: u16, month: u8) -> u8 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if Self::is_leap(year) => 29,
            2 => 28,
            _ => 0,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.month >= 1
            && self.month <= 12
            && self.day >= 1
            && self.day <= Self::days_in_month(self.year, self.month)
    }

    /// The next calendar day. Used only when the local clock runs past midnight
    /// without hearing from the broker.
    #[must_use]
    pub fn next_day(self) -> Self {
        if self.day < Self::days_in_month(self.year, self.month) {
            Self {
                day: self.day + 1,
                ..self
            }
        } else if self.month < 12 {
            Self {
                year: self.year,
                month: self.month + 1,
                day: 1,
            }
        } else {
            Self {
                year: self.year.saturating_add(1),
                month: 1,
                day: 1,
            }
        }
    }
}

/// Local wall-clock time: a date plus a position within the day.
///
/// **The offset is recorded and never applied.** Home Assistant publishes its
/// own local time, and the feeders live in the same house, so the wall-clock
/// fields already arrive in the frame the schedule is written in — `08:00` in a
/// slot means 08:00 on the kitchen wall. There is nothing to convert to.
///
/// Not applying it is what makes daylight saving free: in October Home
/// Assistant simply starts sending `+01:00` and the wall-clock fields shift
/// with it, needing no timezone rules on the device.
///
/// It is kept rather than dropped because it is the one clue that the
/// assumption has been broken. An automation switched from `now()` to
/// `utcnow()` would still publish a valid-looking time, and every meal would
/// quietly move by the offset. Logging it at startup makes that visible on the
/// first line of the console instead.
///
/// `None` means the payload carried no recognisable offset, which is itself
/// worth seeing in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wall {
    pub date: Date,
    pub second_of_day: u32,
    /// Minutes east of UTC, as published. Informational only.
    pub offset_minutes: Option<i16>,
}

/// Seconds in a day. No leap-second handling: a leap second is parsed as :59.
const DAY_S: u32 = 24 * 60 * 60;

impl core::fmt::Display for Wall {
    /// ISO 8601, with the offset back on when there was one.
    ///
    /// Used for both the console and the `last_fed` field, so a timestamp reads
    /// the same wherever it turns up.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.date.year,
            self.date.month,
            self.date.day,
            self.second_of_day / 3600,
            (self.second_of_day / 60) % 60,
            self.second_of_day % 60,
        )?;

        match self.offset_minutes {
            None => Ok(()),
            Some(offset) => {
                let (sign, minutes) = if offset < 0 {
                    ('-', -offset)
                } else {
                    ('+', offset)
                };
                write!(f, "{sign}{:02}:{:02}", minutes / 60, minutes % 60)
            }
        }
    }
}

impl Wall {
    pub fn minute_of_day(&self) -> u16 {
        (self.second_of_day / 60) as u16
    }

    /// Advances by whole seconds, rolling the date over as needed.
    ///
    /// The rollover loop is bounded. A monotonic counter far ahead of the last
    /// alignment means something is badly wrong, and spinning through a century
    /// of dates is not a useful response.
    pub fn plus_seconds(self, seconds: u64) -> Option<Self> {
        let total = self.second_of_day as u64 + seconds;
        let days = total / DAY_S as u64;
        if days > MAX_ROLLOVER_DAYS {
            return None;
        }

        let mut date = self.date;
        for _ in 0..days {
            date = date.next_day();
        }
        Some(Self {
            date,
            second_of_day: (total % DAY_S as u64) as u32,
            ..self
        })
    }
}

/// Roughly three years. Past this the local clock is not worth trusting.
const MAX_ROLLOVER_DAYS: u64 = 1_100;

/// Why a `feeder/time` payload could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeError {
    /// Too short, or the fixed separators are not where they must be.
    Malformed,
    /// Syntactically fine but not a real instant, such as the 31st of February.
    OutOfRange,
}

/// Parses `2026-09-14T08:00:00+02:00` into local wall-clock time.
///
/// Handles the spellings that actually arrive: a numeric offset, a trailing
/// `Z`, and the fractional seconds Python's `datetime.isoformat()` emits. The
/// offset is recorded but never applied — see [`Wall`].
///
/// Surrounding whitespace and a pair of wrapping double quotes are tolerated,
/// because the contract is written with quotes and Home Assistant's
/// `{{ now().isoformat() }}` publishes without them.
pub fn parse_time(payload: &[u8]) -> Result<Wall, TimeError> {
    let text = core::str::from_utf8(payload)
        .map_err(|_| TimeError::Malformed)?
        .trim()
        .trim_matches('"')
        .trim();
    let b = text.as_bytes();

    // YYYY-MM-DDTHH:MM:SS is 19 bytes and every separator is fixed.
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return Err(TimeError::Malformed);
    }
    if b[10] != b'T' && b[10] != b't' && b[10] != b' ' {
        return Err(TimeError::Malformed);
    }

    let year = digits(&b[0..4]).ok_or(TimeError::Malformed)?;
    let month = digits(&b[5..7]).ok_or(TimeError::Malformed)? as u8;
    let day = digits(&b[8..10]).ok_or(TimeError::Malformed)? as u8;
    let hour = digits(&b[11..13]).ok_or(TimeError::Malformed)? as u32;
    let minute = digits(&b[14..16]).ok_or(TimeError::Malformed)? as u32;
    let second = digits(&b[17..19]).ok_or(TimeError::Malformed)? as u32;

    let date = Date { year, month, day };
    if !date.is_valid() || hour > 23 || minute > 59 || second > 60 {
        return Err(TimeError::OutOfRange);
    }

    Ok(Wall {
        date,
        // A leap second lands on :59 rather than being rejected.
        second_of_day: hour * 3600 + minute * 60 + second.min(59),
        offset_minutes: parse_offset(&b[19..]),
    })
}

/// Reads the trailing offset, if there is one this understands.
///
/// Never fails. The offset is informational and no decision reads it, so an
/// unfamiliar suffix must cost the unit its time — the one thing it cannot
/// recover on its own while the broker is away.
fn parse_offset(mut rest: &[u8]) -> Option<i16> {
    // Fractional seconds first: `...:00.123456+02:00`.
    if rest.first() == Some(&b'.') {
        let mut at = 1;
        while matches!(rest.get(at), Some(b'0'..=b'9')) {
            at += 1;
        }
        rest = &rest[at..];
    }

    let (sign, body) = match rest.first()? {
        b'Z' | b'z' if rest.len() == 1 => return Some(0),
        b'+' => (1i16, &rest[1..]),
        b'-' => (-1i16, &rest[1..]),
        _ => return None,
    };

    // +HH:MM, +HHMM and +HH are all in the wild.
    let (hours, minutes) = match body.len() {
        5 if body[2] == b':' => (digits(&body[0..2])?, digits(&body[3..5])?),
        4 => (digits(&body[0..2])?, digits(&body[2..4])?),
        2 => (digits(&body[0..2])?, 0),
        _ => return None,
    };
    if hours > 23 || minutes > 59 {
        return None;
    }

    Some(sign * (hours as i16 * 60 + minutes as i16))
}

fn digits(bytes: &[u8]) -> Option<u16> {
    let mut value: u16 = 0;
    for &b in bytes {
        let d = b.checked_sub(b'0')?;
        if d > 9 {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(d as u16)?;
    }
    Some(value)
}

/// How a `feeder/time` message reached this unit.
///
/// MQTT makes this distinction for us and it is the whole basis of
/// [`LocalClock::is_trusted`]. Because the subscription leaves
/// `retain_as_published` off, the broker clears the retain flag on everything
/// it forwards *except* the messages it replays at subscribe time — so the flag
/// means exactly "this was replayed from the broker's store", not "the
/// publisher set retain".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeSource {
    /// Published while this unit was listening, so Home Assistant is alive and
    /// the time is current to within the network round trip.
    Live,
    /// Replayed by the broker on subscribe. Usually under a minute old — but
    /// if Home Assistant has stopped while the broker keeps running, nothing
    /// refreshes it and it can be any age at all.
    Retained,
    /// Read from the DS3231 at boot, with its oscillator-stop flag clear: it
    /// has been ticking on its own crystal since a live time last set it.
    ///
    /// Earns trust like [`TimeSource::Live`], so a unit that reboots while Home
    /// Assistant is down still feeds. Unlike a live time it never *overrides* a
    /// trusted clock: once a live time has arrived, that is the better source,
    /// and the RTC is rewritten from it rather than the other way round. An RTC
    /// whose flag is set never becomes a `TimeSource` at all — see
    /// `ds3231::Reading::trustworthy`.
    Rtc,
    /// Set by hand on the knob. Treated as a live time — it earns trust and
    /// overrides a trusted clock — because somebody standing at the feeder
    /// saying what time it is is as current as a source gets. If Home
    /// Assistant disagrees, its next live time wins in turn.
    Manual,
}

/// Wall-clock time held in RAM and advanced by the monotonic counter.
///
/// Re-aligned on every live `feeder/time` message. Between messages it
/// free-runs, so losing Home Assistant does not stop a unit that was already
/// running — which is the point, since the broker is also the only thing that
/// could ever tell it the time again.
///
/// ## Trust
///
/// A retained time starts the clock but does **not** make it trustworthy, and
/// the schedule does not run until a live message arrives. A retained
/// `feeder/time` is normally under a minute old, but if Home Assistant stops
/// while Mosquitto keeps running it simply stops being refreshed, and the unit
/// has no way to tell an hour-old message from a fresh one. Anchoring to a
/// stale one and feeding from it would work through the whole day's slots at
/// the wrong times — the exact thing [`Scheduler`] exists to prevent.
///
/// So: the clock runs from a retained time, because a wrong time is still
/// useful in a log line, and the schedule waits for proof that somebody is
/// publishing *now*.
///
/// Once trusted, retained messages are ignored outright. A reconnect replays
/// one, and applying it would drag the clock backwards to whatever the broker
/// happens to be holding.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalClock {
    anchor: Option<(u64, Wall)>,
    trusted: bool,
}

/// What an alignment did to the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alignment {
    pub change: Change,
    /// Whether the schedule may run. See [`LocalClock`].
    pub trusted: bool,
    /// True only on the call that first earned trust, so the console says so
    /// once rather than every minute.
    pub armed_now: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The clock had never been set.
    Started,
    /// Moved by this many seconds, positive if the clock went forward.
    Adjusted { drift_s: i64 },
    /// A retained time arrived after the clock was already trusted, and was
    /// discarded rather than dragging the clock back to it.
    IgnoredStale,
}

impl LocalClock {
    pub const fn new() -> Self {
        Self {
            anchor: None,
            trusted: false,
        }
    }

    /// Pins wall-clock time to a reading of the monotonic counter.
    pub fn align(&mut self, monotonic_ms: u64, wall: Wall, source: TimeSource) -> Alignment {
        // Only a live or hand-set time may move a clock that is already
        // trusted. A replay would drag it back to whatever the broker holds,
        // and an RTC read is never fresher than the time that set it.
        if matches!(source, TimeSource::Retained | TimeSource::Rtc) && self.trusted {
            return Alignment {
                change: Change::IgnoredStale,
                trusted: true,
                armed_now: false,
            };
        }

        let trusting = matches!(
            source,
            TimeSource::Live | TimeSource::Rtc | TimeSource::Manual
        );
        let armed_now = trusting && !self.trusted;
        self.trusted |= trusting;

        let before = self.now(monotonic_ms);
        self.anchor = Some((monotonic_ms, wall));

        Alignment {
            change: match before {
                None => Change::Started,
                Some(old) => Change::Adjusted {
                    drift_s: seconds_between(old, wall),
                },
            },
            trusted: self.trusted,
            armed_now,
        }
    }

    /// Local time now, or `None` if nothing has ever said what time it is.
    ///
    /// `None` is the correct answer to "what time is it" on a unit that has
    /// been power-cycled with no broker. It waits; it does not guess.
    ///
    /// A value here does **not** mean the schedule may run — check
    /// [`Self::is_trusted`] for that.
    pub fn now(&self, monotonic_ms: u64) -> Option<Wall> {
        let (anchored_at, wall) = self.anchor?;
        wall.plus_seconds(monotonic_ms.saturating_sub(anchored_at) / 1_000)
    }

    pub fn is_set(&self) -> bool {
        self.anchor.is_some()
    }

    /// Moves a running clock to `wall` **without changing its trust** — for a
    /// timezone change, where the instant is the same and only the reading of
    /// it moves. [`Self::align`] would let a correction earn trust it has not;
    /// this cannot. Does nothing to a clock that was never set.
    pub fn rezone(&mut self, monotonic_ms: u64, wall: Wall) {
        if self.anchor.is_some() {
            self.anchor = Some((monotonic_ms, wall));
        }
    }

    /// Whether a live time has ever arrived, and so whether the schedule may
    /// run. Never goes back to false: a unit that has been told the time keeps
    /// free-running if Home Assistant disappears.
    pub fn is_trusted(&self) -> bool {
        self.trusted
    }
}

/// Whether `a` is a later calendar day than `b`.
fn later(a: Date, b: Date) -> bool {
    (a.year, a.month, a.day) > (b.year, b.month, b.day)
}

/// Days since 1970-01-01, for exact differences across any span of dates.
///
/// Howard Hinnant's `days_from_civil`, which is exact for the proleptic
/// Gregorian calendar and needs no tables.
pub(crate) fn days_from_civil(date: Date) -> i64 {
    let (m, d) = (date.month as i64, date.day as i64);
    let y = date.year as i64 - (m <= 2) as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date `days` after 1970-01-01: the inverse of [`days_from_civil`].
///
/// Hinnant's `civil_from_days`, exact and table-free like its twin.
pub(crate) fn civil_from_days(days: i64) -> Date {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + (month <= 2) as i64) as u16;
    Date { year, month, day }
}

impl Wall {
    /// Seconds since 1970-01-01 00:00 **on this wall clock** — not UTC unless
    /// the offset is zero. Subtract the offset for the real instant.
    pub fn local_seconds(&self) -> i64 {
        days_from_civil(self.date) * DAY_S as i64 + self.second_of_day as i64
    }

    /// The wall time `local_seconds` from the epoch on a clock at `offset`.
    pub fn from_local_seconds(local_seconds: i64, offset_minutes: Option<i16>) -> Self {
        Self {
            date: civil_from_days(local_seconds.div_euclid(DAY_S as i64)),
            second_of_day: local_seconds.rem_euclid(DAY_S as i64) as u32,
            offset_minutes,
        }
    }
}

/// Signed difference in seconds, `to - from`, exact across any dates.
///
/// Used for the drift a clock correction reports, and by `ds3231::needs_set`
/// to decide whether the RTC is far enough out to rewrite — which is why it is
/// exact: it used to count any change of date as one day, so an RTC a year out
/// could come out as two seconds and be left alone.
pub fn seconds_between(from: Wall, to: Wall) -> i64 {
    (days_from_civil(to.date) - days_from_civil(from.date)) * DAY_S as i64 + to.second_of_day as i64
        - from.second_of_day as i64
}

/// One feeding time. `minute_of_day` is local wall-clock, matching [`Wall`].
///
/// **Zero portions is a meal switched off**, kept in its place rather than
/// removed. Home Assistant's `Meal n` entities address slots by position, so
/// deleting one would renumber every meal after it under the user's feet;
/// switching it off leaves the others where they were. A disabled slot is
/// never fed, never counted in [`Schedule::meals`], and never shown as the
/// next feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub minute_of_day: u16,
    pub portions: u8,
}

impl Slot {
    /// What a gap is padded with: midnight, switched off.
    const OFF: Self = Self {
        minute_of_day: 0,
        portions: 0,
    };

    /// Whether this slot feeds at all. See [`Slot`] for zero.
    pub fn is_enabled(&self) -> bool {
        self.portions > 0
    }
}

/// A change to one slot, by position: what Home Assistant's `Meal n time` and
/// `Meal n portions` entities send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotEdit {
    /// Zero-based. `Meal 1` is index 0.
    pub index: u8,
    pub change: SlotChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotChange {
    Time(u16),
    Portions(u8),
}

/// Why a [`SlotEdit`] was not applied. The schedule is left as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditError {
    /// A position past [`MAX_SLOTS`], or a topic that names no slot.
    NoSuchSlot,
    /// Portions for a meal with no time yet. Set its time first.
    NoTimeYet,
    /// A time or a count that will not parse, or is out of range.
    BadValue,
}

impl SlotEdit {
    /// Reads one from the tail of a command topic and its payload.
    ///
    /// `path` is what follows `feeder/<id>/meal/`: `3/time` or `3/portions`,
    /// one-based to match the entity names. A time is `HH:MM` or `HH:MM:SS`,
    /// which is what Home Assistant's `time` entity sends; seconds are dropped,
    /// because a slot is a minute of the day. Portions are a bare integer.
    pub fn parse(path: &str, payload: &[u8]) -> Result<Self, EditError> {
        let (number, field) = path.split_once('/').ok_or(EditError::NoSuchSlot)?;
        let number = digits(number.as_bytes())
            .filter(|n| !number.starts_with('0') && (1..=MAX_SLOTS as u16).contains(n))
            .ok_or(EditError::NoSuchSlot)?;
        let index = (number - 1) as u8;

        let payload = core::str::from_utf8(payload)
            .map_err(|_| EditError::BadValue)?
            .trim();
        let change = match field {
            "time" => SlotChange::Time(parse_slot_time(payload)?),
            "portions" => {
                SlotChange::Portions(payload.parse::<u8>().map_err(|_| EditError::BadValue)?)
            }
            _ => return Err(EditError::NoSuchSlot),
        };
        Ok(Self { index, change })
    }
}

/// `08:00` or `08:00:00` into minutes since midnight.
pub(crate) fn parse_slot_time(text: &str) -> Result<u16, EditError> {
    let hhmm = match text.len() {
        5 => text,
        8 if text.as_bytes()[5] == b':'
            && digits(&text.as_bytes()[6..]).is_some_and(|s| s < 60) =>
        {
            &text[..5]
        }
        _ => return Err(EditError::BadValue),
    };
    parse_hhmm(hhmm).map_err(|_| EditError::BadValue)
}

/// What the schedule task is told to do with the schedule it owns.
///
/// Queued rather than signalled: a `Signal` holds one value, so a time and a
/// portion count sent a moment apart would leave only the second, and the
/// first edit would vanish without a word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleCommand {
    /// A whole schedule, from `feeder/<id>/schedule` or `feeder/all/schedule`.
    Replace(Schedule),
    /// One slot, from a `Meal n` entity.
    Edit(SlotEdit),
}

impl ScheduleCommand {
    /// The schedule this command asks for, given the one held now — an empty
    /// one for a unit never given any, so a blank unit can be given its first
    /// meal slot by slot. A refused edit leaves `held` as it was.
    pub fn apply(self, held: &Schedule) -> Result<Schedule, EditError> {
        match self {
            Self::Replace(schedule) => Ok(schedule),
            Self::Edit(edit) => held.edited(edit),
        }
    }
}

/// Why a schedule command's payload could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    /// Not the JSON this contract describes.
    Malformed,
    /// More than [`MAX_SLOTS`] entries. Rejected whole rather than truncated:
    /// a silently shortened schedule skips meals with nothing to show for it.
    TooManySlots,
    /// A `time` that is not `HH:MM`, or a `portions` above 255.
    BadSlot,
}

/// The feeding schedule. Owned by the unit: kept in flash, replaced by a
/// `feeder/<id>/schedule` or `feeder/all/schedule` command, and echoed on
/// `feeder/<id>/schedule/state`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schedule {
    slots: Vec<Slot, MAX_SLOTS>,
}

impl Schedule {
    pub const fn new() -> Self {
        Self { slots: Vec::new() }
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Meals a day that will actually feed: slots with at least one portion.
    /// `0` is the number that says a healthy-looking unit never will.
    pub fn meals(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_enabled()).count()
    }

    /// This schedule with one slot changed, or why not.
    ///
    /// Slots are addressed by position, which is how Home Assistant's `Meal n`
    /// entities and the knob number them. Two rules decide what a position
    /// past the end means:
    ///
    /// - **Setting a time past the end adds the meal switched off**, padding
    ///   any gap with switched-off slots at midnight. A time alone never feeds;
    ///   it takes a portion count as well.
    /// - **Setting portions past the end is refused.** There is no time to feed
    ///   them at, and inventing one — midnight, say — would dispense a meal
    ///   nobody chose.
    pub fn edited(&self, edit: SlotEdit) -> Result<Self, EditError> {
        let index = edit.index as usize;
        if index >= MAX_SLOTS {
            return Err(EditError::NoSuchSlot);
        }

        let mut schedule = self.clone();
        match edit.change {
            SlotChange::Time(minute_of_day) => {
                if minute_of_day >= 24 * 60 {
                    return Err(EditError::BadValue);
                }
                while schedule.slots.len() <= index {
                    // Cannot fail: `index < MAX_SLOTS`, checked above.
                    let _ = schedule.slots.push(Slot::OFF);
                }
                schedule.slots[index].minute_of_day = minute_of_day;
            }
            SlotChange::Portions(portions) => {
                let slot = schedule.slots.get_mut(index).ok_or(EditError::NoTimeYet)?;
                slot.portions = portions;
            }
        }
        Ok(schedule)
    }

    /// Parses `[{"time":"08:00","portions":2},{"time":"19:00","portions":2}]`.
    ///
    /// Hand-rolled rather than pulling in a JSON crate, because the shape is
    /// fixed and machine-generated. It is written to reject rather than assume:
    /// every malformed input is an error, never a partial schedule, since a
    /// half-read schedule feeds the wrong meals rather than none.
    ///
    /// Unknown keys are skipped, so adding a field to the contract later does
    /// not brick a unit running older firmware.
    pub fn parse(payload: &[u8]) -> Result<Self, ScheduleError> {
        let mut json = Json::new(payload);
        let mut slots = Vec::new();

        json.expect(b'[')?;
        if json.take(b']') {
            return json.end().map(|()| Self { slots });
        }

        loop {
            let slot = json.slot()?;
            slots.push(slot).map_err(|_| ScheduleError::TooManySlots)?;

            if json.take(b',') {
                continue;
            }
            json.expect(b']')?;
            break;
        }

        json.end()?;
        Ok(Self { slots })
    }
}

/// Bytes a schedule takes in flash: magic, count, eight slots, CRC.
///
/// Fixed-size, so the record is the same length whatever it holds and an
/// unused slot is zeroes rather than whatever the sector held before.
pub const SCHEDULE_RECORD_LEN: usize = 4 + 1 + MAX_SLOTS * 3 + 4;

/// `FDS` for feeder schedule, and a layout version. A separate record from the
/// credentials' `FDR2`, in its own sector, so neither format has to move for
/// the other.
const SCHEDULE_MAGIC: [u8; 4] = *b"FDS1";

/// The longest `to_json` can produce: eight slots of
/// `{"time":"08:00","portions":255}` plus separators and brackets.
pub const SCHEDULE_JSON_LEN: usize = 2 + MAX_SLOTS * 32;

/// Why a stored schedule could not be read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleRecordError {
    /// Erased flash: this unit has never been given a schedule. The normal
    /// state of a new unit, and of one after a factory reset.
    NotStored,
    /// Something is there and it is not a schedule this firmware wrote, or it
    /// was interrupted mid-write. Treated exactly like `NotStored` by the
    /// caller — a unit with no schedule does not feed, which is the safe way
    /// to be wrong — but worth a different log line.
    Corrupt,
}

impl Schedule {
    /// The flash record for this schedule.
    pub fn encode(&self) -> [u8; SCHEDULE_RECORD_LEN] {
        let mut out = [0u8; SCHEDULE_RECORD_LEN];
        out[0..4].copy_from_slice(&SCHEDULE_MAGIC);
        out[4] = self.slots.len() as u8;
        for (i, slot) in self.slots.iter().enumerate() {
            let at = 5 + i * 3;
            out[at..at + 2].copy_from_slice(&slot.minute_of_day.to_le_bytes());
            out[at + 2] = slot.portions;
        }
        let crc = crate::provisioning::crc32(&out[..SCHEDULE_RECORD_LEN - 4]);
        out[SCHEDULE_RECORD_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Reads a record back, refusing anything it cannot vouch for.
    ///
    /// Every slot is checked as `parse` would check it, so a record that
    /// passes the CRC but holds nonsense — written by a future firmware with
    /// the same magic, say — is still refused rather than fed from.
    pub fn decode(bytes: &[u8]) -> Result<Self, ScheduleRecordError> {
        let bytes = bytes
            .get(..SCHEDULE_RECORD_LEN)
            .ok_or(ScheduleRecordError::Corrupt)?;
        if bytes.iter().all(|b| *b == 0xFF) {
            return Err(ScheduleRecordError::NotStored);
        }
        if bytes[0..4] != SCHEDULE_MAGIC {
            return Err(ScheduleRecordError::Corrupt);
        }
        let crc = u32::from_le_bytes(bytes[SCHEDULE_RECORD_LEN - 4..].try_into().unwrap());
        if crc != crate::provisioning::crc32(&bytes[..SCHEDULE_RECORD_LEN - 4]) {
            return Err(ScheduleRecordError::Corrupt);
        }

        let count = bytes[4] as usize;
        if count > MAX_SLOTS {
            return Err(ScheduleRecordError::Corrupt);
        }
        let mut slots = Vec::new();
        for i in 0..count {
            let at = 5 + i * 3;
            let slot = Slot {
                minute_of_day: u16::from_le_bytes([bytes[at], bytes[at + 1]]),
                portions: bytes[at + 2],
            };
            if slot.minute_of_day >= 24 * 60 {
                return Err(ScheduleRecordError::Corrupt);
            }
            let _ = slots.push(slot);
        }
        Ok(Self { slots })
    }

    /// The schedule in the same JSON the commands carry, for the retained
    /// `feeder/<id>/schedule/state` echo. `parse(to_json(s)) == s`.
    pub fn to_json(&self) -> heapless::String<SCHEDULE_JSON_LEN> {
        use core::fmt::Write as _;
        let mut out = heapless::String::new();
        let _ = out.push('[');
        for (i, slot) in self.slots.iter().enumerate() {
            if i > 0 {
                let _ = out.push(',');
            }
            let _ = write!(
                out,
                r#"{{"time":"{:02}:{:02}","portions":{}}}"#,
                slot.minute_of_day / 60,
                slot.minute_of_day % 60,
                slot.portions
            );
        }
        let _ = out.push(']');
        out
    }
}

/// A cursor over the schedule payload.
struct Json<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Json<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn skip_space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_space();
        self.bytes.get(self.at).copied()
    }

    fn take(&mut self, wanted: u8) -> bool {
        if self.peek() == Some(wanted) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, wanted: u8) -> Result<(), ScheduleError> {
        if self.take(wanted) {
            Ok(())
        } else {
            Err(ScheduleError::Malformed)
        }
    }

    /// Trailing content is an error: a payload this firmware only half
    /// understands is not one it should act on.
    fn end(&mut self) -> Result<(), ScheduleError> {
        if self.peek().is_none() {
            Ok(())
        } else {
            Err(ScheduleError::Malformed)
        }
    }

    fn string(&mut self) -> Result<&'a str, ScheduleError> {
        self.expect(b'"')?;
        let start = self.at;
        while let Some(&byte) = self.bytes.get(self.at) {
            match byte {
                b'"' => {
                    let text = core::str::from_utf8(&self.bytes[start..self.at])
                        .map_err(|_| ScheduleError::Malformed)?;
                    self.at += 1;
                    return Ok(text);
                }
                // No escapes in a time or a key. Refusing them keeps the parser
                // honest rather than silently mis-reading one.
                b'\\' => return Err(ScheduleError::Malformed),
                _ => self.at += 1,
            }
        }
        Err(ScheduleError::Malformed)
    }

    fn number(&mut self) -> Result<u32, ScheduleError> {
        self.skip_space();
        let start = self.at;
        while matches!(self.bytes.get(self.at), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        if start == self.at {
            return Err(ScheduleError::Malformed);
        }

        let mut value: u32 = 0;
        for &digit in &self.bytes[start..self.at] {
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add((digit - b'0') as u32))
                .ok_or(ScheduleError::BadSlot)?;
        }
        Ok(value)
    }

    /// Skips one value of any shape, so an unknown key costs nothing.
    fn skip_value(&mut self) -> Result<(), ScheduleError> {
        match self.peek().ok_or(ScheduleError::Malformed)? {
            b'"' => self.string().map(|_| ()),
            open @ (b'{' | b'[') => {
                let close = if open == b'{' { b'}' } else { b']' };
                let mut depth = 0usize;
                while let Some(&byte) = self.bytes.get(self.at) {
                    match byte {
                        // Strings first: braces inside them are not structure.
                        b'"' => {
                            self.string()?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                if byte != close {
                                    return Err(ScheduleError::Malformed);
                                }
                                self.at += 1;
                                return Ok(());
                            }
                        }
                        _ => {}
                    }
                    self.at += 1;
                }
                Err(ScheduleError::Malformed)
            }
            // A number, or one of true/false/null.
            _ => {
                while matches!(self.bytes.get(self.at), Some(byte)
                    if !matches!(byte, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r'))
                {
                    self.at += 1;
                }
                Ok(())
            }
        }
    }

    fn slot(&mut self) -> Result<Slot, ScheduleError> {
        self.expect(b'{')?;

        let mut minute_of_day = None;
        let mut portions = None;

        loop {
            let key = self.string()?;
            self.expect(b':')?;

            match key {
                "time" => minute_of_day = Some(parse_hhmm(self.string()?)?),
                "portions" => {
                    portions =
                        Some(u8::try_from(self.number()?).map_err(|_| ScheduleError::BadSlot)?)
                }
                _ => self.skip_value()?,
            }

            if self.take(b',') {
                continue;
            }
            self.expect(b'}')?;
            break;
        }

        Ok(Slot {
            minute_of_day: minute_of_day.ok_or(ScheduleError::BadSlot)?,
            portions: portions.ok_or(ScheduleError::BadSlot)?,
        })
    }
}

/// `"08:00"` into minutes since midnight.
fn parse_hhmm(text: &str) -> Result<u16, ScheduleError> {
    let b = text.as_bytes();
    if b.len() != 5 || b[2] != b':' {
        return Err(ScheduleError::BadSlot);
    }

    let hour = digits(&b[0..2]).ok_or(ScheduleError::BadSlot)?;
    let minute = digits(&b[3..5]).ok_or(ScheduleError::BadSlot)?;
    if hour > 23 || minute > 59 {
        return Err(ScheduleError::BadSlot);
    }

    Ok(hour * 60 + minute)
}

/// What the scheduler decided this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// No slot is outstanding.
    Nothing,
    /// Feed this many portions. The slot is already marked consumed, so this is
    /// returned exactly once however often the caller asks again.
    Feed { minute_of_day: u16, portions: u8 },
    /// A slot was resolved without feeding. It will not come back.
    Consumed { minute_of_day: u16, why: Skipped },
}

/// Why a slot was marked consumed rather than fed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skipped {
    /// The very first look at the clock, which only takes a baseline.
    Baseline,
    /// The unit is paused. Resuming must not replay it.
    Paused,
    /// Resolved more than [`MAX_LATENESS_S`] after its time, so the unit either
    /// was not running or its clock just jumped. Not a meal to catch up.
    TooLate { by_s: u32 },
}

/// Decides when to feed, and refuses to feed twice.
#[derive(Debug, Clone, Default)]
pub struct Scheduler {
    schedule: Schedule,
    /// The day and time-of-day of the last slot resolved, fed or not.
    consumed_through: Option<(Date, u16)>,
    /// False until the first look at the clock. See the module docs.
    baselined: bool,
    /// Whether any schedule has arrived at all — an empty one included.
    ///
    /// The baseline is only meaningful against a schedule. Normally the one
    /// in flash is set before the clock is trusted, so the first look has it.
    /// But a unit with nothing stored can have its clock trusted — by the RTC,
    /// a second after boot — long before a schedule command arrives, and a
    /// baseline spent on no slots would leave the first real schedule to the
    /// lateness limit alone: given one at 19:01, it would serve the 19:00
    /// meal. So the first look waits for a schedule, however early the clock
    /// was trusted. Found when the RTC first armed ahead of the broker's
    /// replay, back when the schedule lived there.
    received: bool,
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            schedule: Schedule::new(),
            consumed_through: None,
            baselined: false,
            received: false,
        }
    }

    /// Replaces the schedule.
    ///
    /// Deliberately does **not** clear the consumed marker. The marker is a
    /// time of day, so it stays meaningful across a schedule that gains or
    /// loses slots — which is exactly why it is not an index.
    pub fn set_schedule(&mut self, schedule: Schedule) {
        self.schedule = schedule;
        self.received = true;
    }

    pub fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    /// Resolves at most one slot per call.
    ///
    /// Call it whenever convenient — every second is plenty. Calling it more
    /// often changes nothing, because a resolved slot is marked consumed before
    /// this returns.
    pub fn next_due(&mut self, now: Wall, paused: bool) -> Due {
        // No schedule yet: nothing is due, and — crucially — this is not the
        // first look. See `received`.
        if !self.received {
            return Due::Nothing;
        }

        let after = match self.consumed_through {
            // A later day re-arms every slot: yesterday's marker says nothing
            // about today. Only *forward* — see below.
            Some((date, _)) if later(now.date, date) => None,
            // The same day, or a clock that has gone **backwards** across
            // midnight. The second is not a new day, it is a clock being
            // corrected — a date set wrong by hand on the knob, then put right
            // by Home Assistant — and treating it as a fresh day would serve
            // again a meal served minutes earlier. So the marker's time of day
            // still holds: nothing at or before the last slot resolved fires
            // again. The price is that a clock set *back* by a day skips that
            // day's earlier meals, which is the direction this project always
            // chooses.
            Some((date, minute)) => {
                // Moved back: re-date the marker to today, keeping its time of
                // day. Left dated in the future it would hold every slot up to
                // that minute on every earlier day — a year set one too far,
                // then corrected, would skip those meals for a year. Re-dated,
                // the correction still cannot repeat a meal, and the next
                // midnight re-arms as usual.
                if later(date, now.date) {
                    self.consumed_through = Some((now.date, minute));
                }
                Some(minute)
            }
            None => None,
        };

        let now_minute = now.minute_of_day();
        let due = self
            .schedule
            .slots()
            .iter()
            .filter(|slot| slot.is_enabled())
            .filter(|slot| slot.minute_of_day <= now_minute)
            .filter(|slot| after.is_none_or(|limit| slot.minute_of_day > limit))
            // The latest outstanding slot. Anything earlier is a missed meal,
            // and missed meals are not caught up.
            .max_by_key(|slot| slot.minute_of_day)
            .copied();

        let Some(slot) = due else {
            // Nothing outstanding, but the clock has now been seen.
            self.baselined = true;
            return Due::Nothing;
        };

        self.consumed_through = Some((now.date, slot.minute_of_day));
        let minute_of_day = slot.minute_of_day;

        if !self.baselined {
            self.baselined = true;
            return Due::Consumed {
                minute_of_day,
                why: Skipped::Baseline,
            };
        }

        let lateness_s = now.second_of_day - slot.minute_of_day as u32 * 60;
        if lateness_s > MAX_LATENESS_S {
            return Due::Consumed {
                minute_of_day,
                why: Skipped::TooLate { by_s: lateness_s },
            };
        }

        if paused {
            return Due::Consumed {
                minute_of_day,
                why: Skipped::Paused,
            };
        }

        Due::Feed {
            minute_of_day,
            portions: slot.portions,
        }
    }

    /// The next slot that will fire, without resolving anything.
    ///
    /// For the display, and **deliberately `&self`**. [`next_due`] marks a slot
    /// consumed before it returns, so painting a screen with it would eat the
    /// meal it was describing — the one bug this whole module exists to
    /// prevent, arriving through the back door.
    ///
    /// Slots strictly *after* now, so a schedule that has run out today rolls
    /// to tomorrow's earliest. A slot at exactly this minute is left out
    /// because [`next_due`] is about to resolve it anyway: it is not the next
    /// feed, it is this one.
    ///
    /// Says nothing about whether the unit *will* feed. A paused or
    /// clock-less unit has an upcoming slot and will not act on it, and
    /// deciding that is the caller's job — `display.rs` suppresses the line
    /// rather than promising a meal that is not coming.
    ///
    /// [`next_due`]: Self::next_due
    pub fn upcoming(&self, now: Wall) -> Option<Slot> {
        let now_minute = now.minute_of_day();

        let enabled = || {
            self.schedule
                .slots()
                .iter()
                .filter(|slot| slot.is_enabled())
        };

        enabled()
            .filter(|slot| slot.minute_of_day > now_minute)
            .min_by_key(|slot| slot.minute_of_day)
            // Nothing left today, so the next one is tomorrow's first.
            .or_else(|| enabled().min_by_key(|slot| slot.minute_of_day))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: u16, month: u8, day: u8) -> Date {
        Date { year, month, day }
    }

    fn wall(day: u8, hour: u32, minute: u32) -> Wall {
        wall_s(day, hour, minute, 0)
    }

    fn wall_s(day: u8, hour: u32, minute: u32, second: u32) -> Wall {
        Wall {
            date: date(2026, 9, day),
            second_of_day: hour * 3600 + minute * 60 + second,
            offset_minutes: None,
        }
    }

    /// The schedule Home Assistant publishes in the contract: 08:00 and 19:00.
    fn two_meals() -> Scheduler {
        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(
            Schedule::parse(br#"[{"time":"08:00","portions":2},{"time":"19:00","portions":3}]"#)
                .unwrap(),
        );
        scheduler
    }

    /// Takes the baseline the way the real task does, before any slot is due.
    fn armed_at(scheduler: &mut Scheduler, now: Wall) {
        assert_eq!(scheduler.next_due(now, false), Due::Nothing);
    }

    // ---- upcoming, for the display ----

    #[test]
    fn upcoming_is_the_next_slot_later_today() {
        let scheduler = two_meals();

        assert_eq!(
            scheduler.upcoming(wall(17, 12, 0)),
            Some(Slot {
                minute_of_day: 19 * 60,
                portions: 3
            })
        );
    }

    #[test]
    fn upcoming_rolls_to_tomorrow_once_the_day_is_done() {
        let scheduler = two_meals();

        assert_eq!(
            scheduler.upcoming(wall(17, 21, 0)),
            Some(Slot {
                minute_of_day: 8 * 60,
                portions: 2
            }),
            "after the last meal, the next one is tomorrow's first"
        );
    }

    #[test]
    fn upcoming_skips_the_slot_that_is_due_this_very_minute() {
        let scheduler = two_meals();

        // 08:00 is not the *next* feed, it is this one: `next_due` is about to
        // resolve it. Reporting it would leave the screen claiming a future
        // meal at a time that has already arrived.
        assert_eq!(
            scheduler.upcoming(wall(17, 8, 0)),
            Some(Slot {
                minute_of_day: 19 * 60,
                portions: 3
            })
        );
    }

    #[test]
    fn upcoming_has_no_answer_without_a_schedule() {
        assert_eq!(Scheduler::new().upcoming(wall(17, 12, 0)), None);
    }

    /// The whole reason this is a separate method rather than a call to
    /// `next_due`: painting a screen must never eat a meal.
    #[test]
    fn upcoming_resolves_nothing() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(17, 7, 0));

        for _ in 0..50 {
            scheduler.upcoming(wall(17, 7, 30));
        }

        // 08:00 still fires, exactly as if nothing had asked.
        assert_eq!(
            scheduler.next_due(wall(17, 8, 0), false),
            Due::Feed {
                minute_of_day: 8 * 60,
                portions: 2
            }
        );
    }

    // ---- dates ----

    #[test]
    fn next_day_walks_through_a_month() {
        assert_eq!(date(2026, 9, 14).next_day(), date(2026, 9, 15));
        assert_eq!(date(2026, 9, 30).next_day(), date(2026, 10, 1));
        assert_eq!(date(2026, 12, 31).next_day(), date(2027, 1, 1));
    }

    #[test]
    fn february_knows_about_leap_years() {
        assert_eq!(date(2026, 2, 28).next_day(), date(2026, 3, 1));
        assert_eq!(date(2028, 2, 28).next_day(), date(2028, 2, 29));
        assert_eq!(date(2028, 2, 29).next_day(), date(2028, 3, 1));
        // 2100 is divisible by 4 but not a leap year.
        assert_eq!(date(2100, 2, 28).next_day(), date(2100, 3, 1));
        assert_eq!(date(2000, 2, 28).next_day(), date(2000, 2, 29));
    }

    // ---- parsing time ----

    #[test]
    fn parses_the_time_home_assistant_publishes() {
        let parsed = parse_time(b"2026-09-14T08:00:00+02:00").unwrap();
        assert_eq!(parsed.date, date(2026, 9, 14));
        assert_eq!(parsed.second_of_day, 8 * 3600);
    }

    #[test]
    fn tolerates_the_spellings_that_actually_arrive() {
        // Every one of these means eight in the morning, whatever it says about
        // the zone, so the wall-clock fields must come out identical.
        for payload in [
            br#""2026-09-14T08:00:00+02:00""#.as_slice(), // quoted, as the contract writes it
            b"2026-09-14T08:00:00.123456+02:00",          // microseconds, as isoformat emits
            b"2026-09-14T08:00:00Z",
            b"2026-09-14 08:00:00+02:00", // space separator
            b"  2026-09-14T08:00:00Z\n",  // surrounding whitespace
            b"2026-09-14T08:00:00+0200",  // no colon in the offset
            b"2026-09-14T08:00:00",       // no offset at all
        ] {
            let parsed = parse_time(payload).unwrap_or_else(|e| panic!("{payload:?}: {e:?}"));
            assert_eq!(parsed.date, date(2026, 9, 14), "{payload:?}");
            assert_eq!(parsed.second_of_day, 8 * 3600, "{payload:?}");
        }
    }

    #[test]
    fn reads_the_offsets_that_exist_in_the_world() {
        let offset = |text: &[u8]| parse_time(text).unwrap().offset_minutes;

        assert_eq!(offset(b"2026-09-14T08:00:00Z"), Some(0));
        assert_eq!(offset(b"2026-09-14T08:00:00+00:00"), Some(0));
        // India, Newfoundland and the Chatham Islands: not whole hours.
        assert_eq!(offset(b"2026-09-14T08:00:00+05:30"), Some(330));
        assert_eq!(offset(b"2026-09-14T08:00:00-03:30"), Some(-210));
        assert_eq!(offset(b"2026-09-14T08:00:00+12:45"), Some(765));
        // Hour-only spelling.
        assert_eq!(offset(b"2026-09-14T08:00:00-05"), Some(-300));
    }

    #[test]
    fn an_unreadable_offset_never_costs_the_time() {
        // The offset feeds no decision. Losing the time would stop the schedule,
        // and the unit cannot recover it alone while the broker is away.
        let parsed = parse_time(b"2026-09-14T08:00:00 CEST").unwrap();
        assert_eq!(parsed.second_of_day, 8 * 3600);
        assert_eq!(parsed.offset_minutes, None);
    }

    #[test]
    fn a_time_prints_back_the_way_it_arrived() {
        for text in [
            "2026-09-14T08:00:00+02:00",
            "2026-09-14T08:00:00+00:00",
            "2026-12-31T23:59:59-03:30",
            "2026-09-14T08:00:00", // no offset in, none out
        ] {
            let parsed = parse_time(text.as_bytes()).unwrap();
            assert_eq!(std::format!("{parsed}"), text);
        }

        // `Z` is the one spelling that comes back differently, as +00:00.
        let utc = parse_time(b"2026-09-14T08:00:00Z").unwrap();
        assert_eq!(std::format!("{utc}"), "2026-09-14T08:00:00+00:00");
    }

    #[test]
    fn the_offset_is_carried_across_a_free_run() {
        // It ends up in `last_fed`, so it has to survive the local clock
        // advancing and rolling over midnight.
        let mut clock = LocalClock::new();
        clock.align(
            0,
            parse_time(b"2026-09-14T23:59:30+02:00").unwrap(),
            TimeSource::Live,
        );

        let later = clock.now(60_000).unwrap();
        assert_eq!(later.date, date(2026, 9, 15));
        assert_eq!(later.offset_minutes, Some(120));
    }

    #[test]
    fn the_offset_is_recorded_but_never_applied() {
        // Schedules are local wall-clock times, so 08:00 is 08:00 whatever the
        // offset says. Applying it would move every meal by an hour for half the
        // year. It is kept only so the console can show it.
        let cet = parse_time(b"2026-09-14T08:00:00+01:00").unwrap();
        let cest = parse_time(b"2026-09-14T08:00:00+02:00").unwrap();

        assert_eq!(cet.second_of_day, cest.second_of_day);
        assert_eq!(cet.offset_minutes, Some(60));
        assert_eq!(cest.offset_minutes, Some(120));
    }

    #[test]
    fn rejects_rubbish_rather_than_guessing() {
        for payload in [
            b"".as_slice(),
            b"not a time",
            b"2026-09-14",
            b"2026/09/14T08:00:00",
            b"20260914T080000",
            b"202X-09-14T08:00:00Z",
        ] {
            assert_eq!(
                parse_time(payload),
                Err(TimeError::Malformed),
                "{payload:?}"
            );
        }
    }

    #[test]
    fn rejects_dates_that_do_not_exist() {
        assert_eq!(
            parse_time(b"2026-02-30T08:00:00Z"),
            Err(TimeError::OutOfRange)
        );
        assert_eq!(
            parse_time(b"2026-13-01T08:00:00Z"),
            Err(TimeError::OutOfRange)
        );
        assert_eq!(
            parse_time(b"2026-09-14T25:00:00Z"),
            Err(TimeError::OutOfRange)
        );
        // ...but 29 February in a leap year is fine.
        assert!(parse_time(b"2028-02-29T08:00:00Z").is_ok());
    }

    // ---- the local clock ----

    #[test]
    fn the_clock_says_nothing_until_it_is_told() {
        let clock = LocalClock::new();
        assert!(!clock.is_set());
        assert_eq!(clock.now(123_456), None);
    }

    #[test]
    fn the_clock_free_runs_between_messages() {
        let mut clock = LocalClock::new();
        let started = clock.align(10_000, wall(14, 8, 0), TimeSource::Live);
        assert_eq!(started.change, Change::Started);

        // Ten minutes of monotonic time with no word from Home Assistant.
        let later = clock.now(10_000 + 600_000).unwrap();
        assert_eq!(later, wall(14, 8, 10));
    }

    #[test]
    fn the_clock_rolls_over_midnight_on_its_own() {
        let mut clock = LocalClock::new();
        clock.align(0, wall_s(30, 23, 59, 30), TimeSource::Live);

        let later = clock.now(60_000).unwrap();
        assert_eq!(later.date, date(2026, 10, 1));
        assert_eq!(later.second_of_day, 30);
    }

    #[test]
    fn realignment_reports_the_drift_it_corrected() {
        let mut clock = LocalClock::new();
        clock.align(0, wall(14, 8, 0), TimeSource::Live);

        // A minute of monotonic time passes, but Home Assistant says five did.
        assert_eq!(
            clock.align(60_000, wall(14, 8, 5), TimeSource::Live).change,
            Change::Adjusted { drift_s: 240 }
        );
    }

    // ---- trusting the clock ----

    #[test]
    fn a_retained_time_runs_the_clock_but_does_not_arm_the_schedule() {
        // A retained feeder/time is normally under a minute old. But if Home
        // Assistant stops while Mosquitto keeps running, nothing refreshes it
        // and it can be any age, with nothing to distinguish the two cases.
        let mut clock = LocalClock::new();
        let started = clock.align(10_000, wall(14, 8, 0), TimeSource::Retained);

        assert_eq!(started.change, Change::Started);
        assert!(!started.trusted);
        assert!(!started.armed_now);
        assert!(!clock.is_trusted());

        // The clock still runs, because a time is worth having in a log line.
        assert_eq!(clock.now(10_000 + 60_000).unwrap(), wall(14, 8, 1));
    }

    #[test]
    fn a_live_time_arms_the_schedule_and_says_so_once() {
        let mut clock = LocalClock::new();
        clock.align(10_000, wall(14, 8, 0), TimeSource::Retained);

        let live = clock.align(70_000, wall(14, 8, 2), TimeSource::Live);
        assert!(live.trusted);
        assert!(live.armed_now, "the console should announce this once");
        assert!(clock.is_trusted());

        // ...and not announce it again on every subsequent message.
        let next = clock.align(130_000, wall(14, 8, 3), TimeSource::Live);
        assert!(next.trusted);
        assert!(!next.armed_now);
    }

    #[test]
    fn a_live_time_first_arms_immediately() {
        let mut clock = LocalClock::new();
        let started = clock.align(10_000, wall(14, 8, 0), TimeSource::Live);

        assert_eq!(started.change, Change::Started);
        assert!(started.trusted);
        assert!(started.armed_now);
    }

    #[test]
    fn a_retained_time_after_trust_is_ignored_rather_than_applied() {
        // Every reconnect replays the retained message. Applying it would drag
        // the clock backwards to whatever the broker is holding, which is the
        // whole hazard if Home Assistant has stopped.
        let mut clock = LocalClock::new();
        clock.align(0, wall(14, 8, 0), TimeSource::Live);

        let replayed = clock.align(600_000, wall(14, 6, 0), TimeSource::Retained);

        assert_eq!(replayed.change, Change::IgnoredStale);
        assert!(replayed.trusted, "a stale replay must never disarm a unit");
        // The clock kept free-running instead of jumping back two hours.
        assert_eq!(clock.now(600_000).unwrap(), wall(14, 8, 10));
    }

    /// The double feed the RTC nearly introduced. The clock is trusted before
    /// any schedule arrives; a reboot one minute after the 19:00 meal must not
    /// serve it again when the schedule turns up seconds later.
    #[test]
    fn the_baseline_waits_for_a_schedule_to_arrive() {
        let mut scheduler = Scheduler::new();
        assert_eq!(scheduler.next_due(wall(17, 19, 1), false), Due::Nothing);
        assert_eq!(
            scheduler.next_due(wall_s(17, 19, 1, 3), false),
            Due::Nothing
        );

        scheduler.set_schedule(Schedule::parse(br#"[{"time":"19:00","portions":2}]"#).unwrap());

        assert_eq!(
            scheduler.next_due(wall_s(17, 19, 1, 5), false),
            Due::Consumed {
                minute_of_day: 19 * 60,
                why: Skipped::Baseline,
            },
            "a meal served before the reboot was fed again"
        );
    }

    /// An empty schedule is still a schedule: the first look after it is the
    /// baseline, and a slot added later is judged as a new one.
    #[test]
    fn an_empty_schedule_still_counts_as_received() {
        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(Schedule::parse(b"[]").unwrap());
        assert_eq!(scheduler.next_due(wall(17, 12, 0), false), Due::Nothing);

        scheduler.set_schedule(Schedule::parse(br#"[{"time":"12:01","portions":1}]"#).unwrap());
        assert_eq!(
            scheduler.next_due(wall_s(17, 12, 1, 10), false),
            Due::Feed {
                minute_of_day: 12 * 60 + 1,
                portions: 1
            }
        );
    }

    /// The sequence the knob made possible: a meal served, the date set a day
    /// ahead by hand — which *is* a new day, and feeds — then Home Assistant
    /// putting the date back. The correction must not serve today's meal a
    /// second time.
    #[test]
    fn a_date_corrected_backwards_does_not_serve_a_meal_again() {
        let mut scheduler = two_meals();
        scheduler.next_due(wall(17, 7, 0), false);
        assert!(matches!(
            scheduler.next_due(wall_s(17, 19, 0, 30), false),
            Due::Feed { .. }
        ));

        // Set to tomorrow, 19:00: tomorrow's meal, genuinely due.
        assert!(matches!(
            scheduler.next_due(wall_s(18, 19, 0, 40), false),
            Due::Feed { .. }
        ));

        // Home Assistant puts it back to today, a minute after the meal.
        assert_eq!(scheduler.next_due(wall(17, 19, 1), false), Due::Nothing);

        // And the real tomorrow is a normal day: both meals, not skipped
        // because the marker was once dated the 18th.
        assert!(matches!(
            scheduler.next_due(wall(18, 8, 0), false),
            Due::Feed { .. }
        ));
        assert!(matches!(
            scheduler.next_due(wall(18, 19, 0), false),
            Due::Feed { .. }
        ));
    }

    /// A year set one too far on the knob, a meal served under it, then the
    /// date corrected: the following days feed normally rather than waiting a
    /// year for the calendar to catch up with the marker.
    #[test]
    fn a_year_set_wrong_then_corrected_does_not_skip_a_year_of_meals() {
        let mut scheduler = two_meals();
        let next_year = |day, hour, minute| Wall {
            date: date(2027, 9, day),
            ..wall(day, hour, minute)
        };
        scheduler.next_due(next_year(17, 7, 0), false);
        assert!(matches!(
            scheduler.next_due(next_year(17, 19, 0), false),
            Due::Feed { .. }
        ));

        // Corrected to the real year, the same evening.
        assert_eq!(scheduler.next_due(wall(17, 19, 1), false), Due::Nothing);
        // The next morning and evening, in the real year.
        assert!(matches!(
            scheduler.next_due(wall(18, 8, 0), false),
            Due::Feed { .. }
        ));
        assert!(matches!(
            scheduler.next_due(wall(18, 19, 0), false),
            Due::Feed { .. }
        ));
    }

    #[test]
    fn seconds_between_is_exact_across_any_dates() {
        let at = |y, m, d, s| Wall {
            date: Date {
                year: y,
                month: m,
                day: d,
            },
            second_of_day: s,
            offset_minutes: None,
        };
        assert_eq!(seconds_between(at(2026, 9, 25, 10), at(2026, 9, 25, 13)), 3);
        assert_eq!(
            seconds_between(at(2026, 9, 25, 86_399), at(2026, 9, 26, 1)),
            2
        );
        assert_eq!(
            seconds_between(at(2026, 9, 26, 1), at(2026, 9, 25, 86_399)),
            -2
        );
        // A year apart, which the old one-day rule reported as two seconds.
        assert_eq!(
            seconds_between(at(2025, 9, 25, 86_399), at(2026, 9, 26, 1)),
            365 * 86_400 + 2
        );
        // Across a leap day.
        assert_eq!(
            seconds_between(at(2028, 2, 28, 0), at(2028, 3, 1, 0)),
            2 * 86_400
        );
        assert_eq!(
            seconds_between(at(1970, 1, 1, 0), at(2000, 1, 1, 0)),
            10_957 * 86_400
        );
    }

    /// Midnight still re-arms the day: the forward case is unchanged.
    #[test]
    fn midnight_still_re_arms_every_slot() {
        let mut scheduler = two_meals();
        scheduler.next_due(wall(17, 7, 0), false);
        assert!(matches!(
            scheduler.next_due(wall(17, 8, 0), false),
            Due::Feed { .. }
        ));
        assert!(matches!(
            scheduler.next_due(wall(17, 19, 0), false),
            Due::Feed { .. }
        ));
        assert!(matches!(
            scheduler.next_due(wall(18, 8, 0), false),
            Due::Feed { .. }
        ));
    }

    /// A clock set by hand arms a unit that has never seen a network, and
    /// corrects one that has.
    #[test]
    fn a_hand_set_time_arms_and_overrides() {
        let mut clock = LocalClock::new();
        assert!(clock.align(0, wall(14, 8, 0), TimeSource::Manual).armed_now);

        let mut trusted = LocalClock::new();
        trusted.align(0, wall(14, 8, 0), TimeSource::Live);
        let set = trusted.align(1_000, wall(14, 9, 0), TimeSource::Manual);
        assert_eq!(set.change, Change::Adjusted { drift_s: 3_599 });
        assert_eq!(trusted.now(1_000).unwrap(), wall(14, 9, 0));
    }

    /// Setting the clock *back* across a meal already served must not serve it
    /// again. The consumed marker is keyed on the time of day, so the slot is
    /// still behind it when the clock reaches 19:00 a second time.
    #[test]
    fn setting_the_clock_back_over_a_meal_does_not_repeat_it() {
        let mut scheduler = two_meals();
        scheduler.next_due(wall(17, 7, 0), false);
        assert!(matches!(
            scheduler.next_due(wall(17, 19, 0), false),
            Due::Feed { .. }
        ));

        // Somebody sets it back to 18:55, and the clock walks forward again.
        assert_eq!(scheduler.next_due(wall(17, 18, 55), false), Due::Nothing);
        assert_eq!(scheduler.next_due(wall(17, 19, 0), false), Due::Nothing);
        assert_eq!(scheduler.next_due(wall(17, 19, 1), false), Due::Nothing);
    }

    /// The point of the RTC: a unit rebooted with Home Assistant down arms its
    /// schedule from a clock that has kept time on its own crystal.
    #[test]
    fn a_trustworthy_rtc_arms_the_schedule() {
        let mut clock = LocalClock::new();
        let a = clock.align(0, wall(14, 8, 0), TimeSource::Rtc);

        assert!(a.trusted);
        assert!(a.armed_now);
        assert!(clock.is_trusted());
    }

    /// Once the RTC has armed the clock, a retained replay is as stale as ever.
    #[test]
    fn a_retained_time_after_the_rtc_is_ignored() {
        let mut clock = LocalClock::new();
        clock.align(0, wall(14, 8, 0), TimeSource::Rtc);

        let replayed = clock.align(1_000, wall(12, 0, 0), TimeSource::Retained);

        assert_eq!(replayed.change, Change::IgnoredStale);
        assert_eq!(clock.now(1_000).unwrap(), wall_s(14, 8, 0, 1));
    }

    /// A live time still corrects a clock the RTC started, and does not
    /// announce arming a second time.
    #[test]
    fn a_live_time_corrects_an_rtc_start() {
        let mut clock = LocalClock::new();
        clock.align(0, wall(14, 8, 0), TimeSource::Rtc);

        let live = clock.align(10_000, wall_s(14, 8, 0, 13), TimeSource::Live);

        assert_eq!(live.change, Change::Adjusted { drift_s: 3 });
        assert!(!live.armed_now, "already armed by the RTC");
        assert_eq!(clock.now(10_000).unwrap(), wall_s(14, 8, 0, 13));
    }

    /// The RTC never overrides a live time: the live one is the source the
    /// RTC itself is set from.
    #[test]
    fn an_rtc_read_after_a_live_time_is_ignored() {
        let mut clock = LocalClock::new();
        clock.align(0, wall(14, 8, 0), TimeSource::Live);

        let late = clock.align(1_000, wall(14, 7, 0), TimeSource::Rtc);

        assert_eq!(late.change, Change::IgnoredStale);
        assert_eq!(clock.now(1_000).unwrap(), wall_s(14, 8, 0, 1));
    }

    #[test]
    fn a_clock_too_old_to_project_starts_over_rather_than_re_arming() {
        // `now` gives up past MAX_ROLLOVER_DAYS, so an already-trusted clock
        // that has free-run beyond it reports `Started` again on the next live
        // time — trusted, but not armed, because trust was never lost.
        //
        // Pinned because it is the one path to `Change::Started` with
        // `trusted`, and a reading of `align` that misses it concludes the
        // console's `clock: started, <time>` line is dead code.
        let mut clock = LocalClock::new();
        clock.align(0, wall(14, 8, 0), TimeSource::Live);

        let elapsed_ms = (MAX_ROLLOVER_DAYS + 1) * 24 * 3600 * 1_000;
        assert!(clock.now(elapsed_ms).is_none(), "the anchor must be stale");

        let restarted = clock.align(elapsed_ms, wall(14, 9, 0), TimeSource::Live);

        assert_eq!(restarted.change, Change::Started);
        assert!(restarted.trusted);
        assert!(
            !restarted.armed_now,
            "trust was never lost, so nothing re-arms"
        );
    }

    #[test]
    fn trust_survives_home_assistant_disappearing() {
        // Offline is not the same as untold. A unit that has been told the time
        // keeps feeding on its own clock, which is the documented behaviour.
        let mut clock = LocalClock::new();
        clock.align(0, wall(14, 7, 0), TimeSource::Live);

        assert!(clock.is_trusted());
        // Six hours later, still nothing from Home Assistant.
        assert!(clock.is_trusted());
        assert_eq!(clock.now(6 * 3600 * 1000).unwrap(), wall(14, 13, 0));
    }

    #[test]
    fn days_round_trip_across_centuries() {
        for days in [-719_468, -1, 0, 1, 10_957, 20_724, 47_481, 100_000] {
            assert_eq!(days_from_civil(civil_from_days(days)), days, "{days}");
        }
        assert_eq!(civil_from_days(0), date(1970, 1, 1));
        assert_eq!(civil_from_days(20_724), date(2026, 9, 28));
        assert_eq!(civil_from_days(11_016), date(2000, 2, 29));
    }

    #[test]
    fn a_wall_round_trips_through_local_seconds() {
        let w = Wall {
            offset_minutes: Some(120),
            ..wall_s(28, 23, 59, 59)
        };
        assert_eq!(Wall::from_local_seconds(w.local_seconds(), Some(120)), w);
        let next = Wall::from_local_seconds(w.local_seconds() + 1, Some(120));
        assert_eq!((next.date, next.second_of_day), (date(2026, 9, 29), 0));
    }

    #[test]
    fn rezoning_moves_the_reading_but_never_earns_trust() {
        let mut clock = LocalClock::new();
        clock.rezone(0, wall(28, 8, 0));
        assert!(!clock.is_set());

        clock.align(0, wall(28, 8, 0), TimeSource::Retained);
        clock.rezone(1_000, wall(28, 9, 0));
        assert!(!clock.is_trusted());
        assert_eq!(clock.now(1_000), Some(wall(28, 9, 0)));
    }

    // ---- parsing the schedule ----

    #[test]
    fn parses_the_schedule_home_assistant_publishes() {
        let schedule =
            Schedule::parse(br#"[{"time":"08:00","portions":2},{"time":"19:00","portions":2}]"#)
                .unwrap();

        assert_eq!(
            schedule.slots(),
            [
                Slot {
                    minute_of_day: 480,
                    portions: 2
                },
                Slot {
                    minute_of_day: 1140,
                    portions: 2
                },
            ]
        );
    }

    #[test]
    fn an_empty_schedule_is_valid_and_means_never() {
        let schedule = Schedule::parse(b"[]").unwrap();
        assert!(schedule.is_empty());

        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(schedule);
        armed_at(&mut scheduler, wall(14, 8, 0));
        assert_eq!(scheduler.next_due(wall(14, 12, 0), false), Due::Nothing);
    }

    #[test]
    fn whitespace_and_key_order_do_not_matter() {
        let schedule =
            Schedule::parse(b"[\n  { \"portions\" : 4 , \"time\" : \"07:30\" }\n]").unwrap();

        assert_eq!(
            schedule.slots(),
            [Slot {
                minute_of_day: 450,
                portions: 4
            }]
        );
    }

    #[test]
    fn an_unknown_key_is_skipped_rather_than_fatal() {
        // So adding a field to the contract does not brick older firmware.
        let schedule = Schedule::parse(
            br#"[{"time":"08:00","label":"breakfast","meta":{"x":[1,2]},"portions":2}]"#,
        )
        .unwrap();

        assert_eq!(schedule.len(), 1);
        assert_eq!(schedule.slots()[0].portions, 2);
    }

    #[test]
    fn a_malformed_schedule_is_rejected_whole() {
        // Never a partial schedule: half a schedule feeds the wrong meals.
        for payload in [
            b"".as_slice(),
            b"[",
            b"[{}]",
            br#"[{"time":"08:00"}]"#,
            br#"[{"portions":2}]"#,
            br#"[{"time":"08:00","portions":2}"#,
            br#"[{"time":"08:00","portions":2}] trailing"#,
            br#"{"time":"08:00","portions":2}"#,
            br#"[{"time":"0800","portions":2}]"#,
            br#"[{"time":"25:00","portions":2}]"#,
            br#"[{"time":"08:60","portions":2}]"#,
            br#"[{"time":"08:00","portions":300}]"#,
        ] {
            assert!(Schedule::parse(payload).is_err(), "{payload:?}");
        }
    }

    #[test]
    fn too_many_slots_is_refused_not_truncated() {
        let mut payload = alloc_string("[");
        for i in 0..=MAX_SLOTS {
            if i > 0 {
                payload.push(',');
            }
            payload.push_str(&format!(r#"{{"time":"0{}:00","portions":1}}"#, i % 10));
        }
        payload.push(']');

        assert_eq!(
            Schedule::parse(payload.as_bytes()),
            Err(ScheduleError::TooManySlots)
        );
    }

    fn alloc_string(s: &str) -> std::string::String {
        std::string::String::from(s)
    }

    // ---- editing one slot ----

    fn edit(path: &str, payload: &str) -> SlotEdit {
        SlotEdit::parse(path, payload.as_bytes()).unwrap()
    }

    fn two() -> Schedule {
        Schedule::parse(br#"[{"time":"08:00","portions":2},{"time":"19:00","portions":3}]"#)
            .unwrap()
    }

    #[test]
    fn reads_what_home_assistant_entities_send() {
        assert_eq!(
            edit("1/time", "07:45:00"),
            SlotEdit {
                index: 0,
                change: SlotChange::Time(465)
            }
        );
        assert_eq!(
            edit("8/time", "23:59"),
            SlotEdit {
                index: 7,
                change: SlotChange::Time(1439)
            }
        );
        assert_eq!(
            edit("3/portions", "2"),
            SlotEdit {
                index: 2,
                change: SlotChange::Portions(2)
            }
        );
    }

    #[test]
    fn refuses_edits_it_cannot_place() {
        for (path, payload, why) in [
            ("0/time", "08:00", EditError::NoSuchSlot),
            ("9/time", "08:00", EditError::NoSuchSlot),
            ("01/time", "08:00", EditError::NoSuchSlot),
            ("x/time", "08:00", EditError::NoSuchSlot),
            ("10/time", "08:00", EditError::NoSuchSlot),
            ("1", "08:00", EditError::NoSuchSlot),
            ("1/colour", "08:00", EditError::NoSuchSlot),
            ("1/time", "8:00", EditError::BadValue),
            ("1/time", "24:00:00", EditError::BadValue),
            ("1/time", "08:00:60", EditError::BadValue),
            ("1/time", "08:00:00.5", EditError::BadValue),
            ("1/portions", "2.5", EditError::BadValue),
            ("1/portions", "-1", EditError::BadValue),
            ("1/portions", "256", EditError::BadValue),
            ("1/portions", "", EditError::BadValue),
        ] {
            assert_eq!(
                SlotEdit::parse(path, payload.as_bytes()),
                Err(why),
                "{path} {payload}"
            );
        }
    }

    #[test]
    fn an_edit_changes_one_slot_in_place() {
        let edited = two().edited(edit("2/time", "18:30:00")).unwrap();
        assert_eq!(
            edited.to_json().as_str(),
            r#"[{"time":"08:00","portions":2},{"time":"18:30","portions":3}]"#
        );

        let edited = edited.edited(edit("1/portions", "1")).unwrap();
        assert_eq!(
            edited.to_json().as_str(),
            r#"[{"time":"08:00","portions":1},{"time":"18:30","portions":3}]"#
        );
    }

    /// A time past the end adds a meal that does not feed until it is given
    /// portions, and pads any gap with switched-off slots.
    #[test]
    fn a_time_past_the_end_adds_a_meal_switched_off() {
        let edited = two().edited(edit("4/time", "12:00:00")).unwrap();
        assert_eq!(
            edited.to_json().as_str(),
            concat!(
                r#"[{"time":"08:00","portions":2},{"time":"19:00","portions":3},"#,
                r#"{"time":"00:00","portions":0},{"time":"12:00","portions":0}]"#
            )
        );
        assert_eq!(edited.meals(), 2);

        let fed = edited.edited(edit("4/portions", "1")).unwrap();
        assert_eq!(fed.meals(), 3);
    }

    /// Never a meal at an invented time.
    #[test]
    fn portions_for_a_meal_with_no_time_are_refused() {
        assert_eq!(
            two().edited(edit("3/portions", "2")),
            Err(EditError::NoTimeYet)
        );
        assert_eq!(
            Schedule::new().edited(edit("1/portions", "2")),
            Err(EditError::NoTimeYet)
        );
    }

    #[test]
    fn a_blank_unit_is_given_its_first_meal_by_two_edits() {
        let first = Schedule::new().edited(edit("1/time", "08:00:00")).unwrap();
        assert_eq!(first.meals(), 0);
        let first = first.edited(edit("1/portions", "2")).unwrap();
        assert_eq!(first.meals(), 1);
        assert_eq!(
            first.to_json().as_str(),
            r#"[{"time":"08:00","portions":2}]"#
        );
    }

    #[test]
    fn an_edit_survives_flash() {
        let edited = two().edited(edit("5/time", "13:00")).unwrap();
        assert_eq!(Schedule::decode(&edited.encode()), Ok(edited.clone()));
        assert_eq!(Schedule::parse(edited.to_json().as_bytes()), Ok(edited));
    }

    #[test]
    fn a_command_replaces_or_edits_what_is_held() {
        let replace = ScheduleCommand::Replace(Schedule::new());
        assert_eq!(replace.apply(&two()), Ok(Schedule::new()));

        let edit = ScheduleCommand::Edit(edit("2/portions", "0"));
        assert_eq!(edit.clone().apply(&two()).map(|s| s.meals()), Ok(1));
        assert_eq!(edit.apply(&Schedule::new()), Err(EditError::NoTimeYet));
    }

    /// Every meal `discovery.rs` announces must be addressable, whatever
    /// `MAX_SLOTS` is, and nothing past it.
    #[test]
    fn every_slot_number_parses_and_no_other() {
        for n in 1..=MAX_SLOTS {
            let path = format!("{n}/time");
            assert_eq!(
                SlotEdit::parse(&path, b"08:00").map(|e| e.index as usize),
                Ok(n - 1)
            );
        }
        let past = format!("{}/time", MAX_SLOTS + 1);
        assert_eq!(SlotEdit::parse(&past, b"08:00"), Err(EditError::NoSuchSlot));
    }

    // ---- switched-off slots ----

    fn with_one_off() -> Scheduler {
        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(
            Schedule::parse(br#"[{"time":"08:00","portions":2},{"time":"12:00","portions":0},{"time":"19:00","portions":3}]"#)
                .unwrap(),
        );
        scheduler
    }

    #[test]
    fn a_switched_off_meal_never_feeds_and_is_not_counted() {
        let mut scheduler = with_one_off();
        assert_eq!(scheduler.schedule().meals(), 2);
        assert_eq!(scheduler.schedule().len(), 3);

        armed_at(&mut scheduler, wall(14, 7, 0));
        assert!(matches!(
            scheduler.next_due(wall(14, 8, 0), false),
            Due::Feed { portions: 2, .. }
        ));
        assert_eq!(scheduler.next_due(wall(14, 12, 0), false), Due::Nothing);
        assert_eq!(scheduler.next_due(wall(14, 12, 1), false), Due::Nothing);
        assert!(matches!(
            scheduler.next_due(wall(14, 19, 0), false),
            Due::Feed { portions: 3, .. }
        ));
    }

    #[test]
    fn a_switched_off_meal_is_never_the_next_one() {
        let scheduler = with_one_off();
        assert_eq!(
            scheduler.upcoming(wall(14, 9, 0)).map(|s| s.minute_of_day),
            Some(19 * 60)
        );

        let mut all_off = Scheduler::new();
        all_off.set_schedule(Schedule::parse(br#"[{"time":"08:00","portions":0}]"#).unwrap());
        assert_eq!(all_off.upcoming(wall(14, 9, 0)), None);
        assert_eq!(all_off.schedule().meals(), 0);
    }

    // ---- the double-feed guard ----

    #[test]
    fn the_first_look_at_the_clock_never_feeds() {
        // The guard lives in RAM, so a reboot just after 08:00 would otherwise
        // dispense the 08:00 slot for a second time.
        let mut scheduler = two_meals();

        assert_eq!(
            scheduler.next_due(wall_s(14, 8, 0, 30), false),
            Due::Consumed {
                minute_of_day: 480,
                why: Skipped::Baseline
            }
        );
        // And it stays consumed.
        assert_eq!(scheduler.next_due(wall_s(14, 8, 1, 0), false), Due::Nothing);
    }

    #[test]
    fn a_slot_fires_once_when_the_clock_reaches_it() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));

        assert_eq!(
            scheduler.next_due(wall(14, 8, 0), false),
            Due::Feed {
                minute_of_day: 480,
                portions: 2
            }
        );

        // Asked again a second later, and a minute later, and nothing happens.
        assert_eq!(scheduler.next_due(wall_s(14, 8, 0, 1), false), Due::Nothing);
        assert_eq!(scheduler.next_due(wall(14, 8, 1), false), Due::Nothing);
    }

    #[test]
    fn the_second_meal_of_the_day_still_fires() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        scheduler.next_due(wall(14, 8, 0), false);

        assert_eq!(
            scheduler.next_due(wall(14, 19, 0), false),
            Due::Feed {
                minute_of_day: 1140,
                portions: 3
            }
        );
    }

    #[test]
    fn a_new_day_rearms_every_slot() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        scheduler.next_due(wall(14, 8, 0), false);
        scheduler.next_due(wall(14, 19, 0), false);

        assert_eq!(
            scheduler.next_due(wall(15, 8, 0), false),
            Due::Feed {
                minute_of_day: 480,
                portions: 2
            }
        );
    }

    #[test]
    fn a_clock_jump_past_a_slot_does_not_catch_it_up() {
        // The unit's clock was wrong and Home Assistant corrected it. That is
        // not the same as the slot falling due.
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 0));

        assert_eq!(
            scheduler.next_due(wall(14, 12, 0), false),
            Due::Consumed {
                minute_of_day: 480,
                why: Skipped::TooLate { by_s: 4 * 60 * 60 }
            }
        );
        // And it is not waiting to fire later either.
        assert_eq!(scheduler.next_due(wall(14, 12, 1), false), Due::Nothing);
    }

    #[test]
    fn a_clock_jump_past_a_slot_already_fed_changes_nothing() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        scheduler.next_due(wall(14, 8, 0), false);

        assert_eq!(scheduler.next_due(wall(14, 8, 5), false), Due::Nothing);
    }

    #[test]
    fn a_slot_a_little_late_still_feeds() {
        // The broker going quiet for a minute must not cost a meal.
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));

        assert_eq!(
            scheduler.next_due(wall_s(14, 8, 1, 0), false),
            Due::Feed {
                minute_of_day: 480,
                portions: 2
            }
        );
    }

    #[test]
    fn the_lateness_limit_is_where_it_says_it_is() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        assert!(matches!(
            scheduler.next_due(wall_s(14, 8, 0, MAX_LATENESS_S), false),
            Due::Feed { .. }
        ));

        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        assert!(matches!(
            scheduler.next_due(wall_s(14, 8, 0, MAX_LATENESS_S + 1), false),
            Due::Consumed {
                why: Skipped::TooLate { .. },
                ..
            }
        ));
    }

    // ---- pause ----

    #[test]
    fn a_paused_slot_is_consumed_rather_than_fed() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));

        assert_eq!(
            scheduler.next_due(wall(14, 8, 0), true),
            Due::Consumed {
                minute_of_day: 480,
                why: Skipped::Paused
            }
        );
    }

    #[test]
    fn resuming_never_replays_the_slot_that_was_paused_over() {
        // Otherwise every unpause dispenses the meal that was deliberately
        // skipped, which is the whole reason slots are consumed rather than
        // left outstanding.
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        scheduler.next_due(wall(14, 8, 0), true);

        assert_eq!(scheduler.next_due(wall(14, 8, 30), false), Due::Nothing);
    }

    #[test]
    fn resuming_leaves_later_slots_alone() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        scheduler.next_due(wall(14, 8, 0), true);

        assert_eq!(
            scheduler.next_due(wall(14, 19, 0), false),
            Due::Feed {
                minute_of_day: 1140,
                portions: 3
            }
        );
    }

    #[test]
    fn a_unit_paused_across_a_whole_day_feeds_nothing() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 0));

        for hour in 8..24 {
            match scheduler.next_due(wall(14, hour, 0), true) {
                Due::Feed { .. } => panic!("fed while paused at {hour}:00"),
                _ => continue,
            }
        }
    }

    // ---- changing the schedule ----

    #[test]
    fn removing_an_earlier_slot_does_not_cost_the_later_one() {
        // The consumed marker is a time of day, not an index. With an index,
        // dropping the 07:00 slot would shift 19:00 down onto the index already
        // marked consumed, and the evening meal would silently vanish.
        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(
            Schedule::parse(
                br#"[{"time":"07:00","portions":1},{"time":"08:00","portions":2},{"time":"19:00","portions":3}]"#,
            )
            .unwrap(),
        );
        armed_at(&mut scheduler, wall(14, 6, 0));
        scheduler.next_due(wall(14, 8, 0), false);

        scheduler.set_schedule(
            Schedule::parse(br#"[{"time":"08:00","portions":2},{"time":"19:00","portions":3}]"#)
                .unwrap(),
        );

        assert_eq!(
            scheduler.next_due(wall(14, 19, 0), false),
            Due::Feed {
                minute_of_day: 1140,
                portions: 3
            }
        );
    }

    #[test]
    fn a_slot_inserted_earlier_in_the_day_is_not_fed_retroactively() {
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 59));
        scheduler.next_due(wall(14, 8, 0), false);

        // Home Assistant adds a midday meal while the day is already past it.
        scheduler.set_schedule(
            Schedule::parse(
                br#"[{"time":"08:00","portions":2},{"time":"12:00","portions":1},{"time":"19:00","portions":3}]"#,
            )
            .unwrap(),
        );

        assert_eq!(
            scheduler.next_due(wall(14, 13, 0), false),
            Due::Consumed {
                minute_of_day: 720,
                why: Skipped::TooLate { by_s: 3600 }
            }
        );
    }

    /// Runs a whole day the way the task does, one call a second, and returns
    /// every feed it produced as `(minute_of_day, portions)`.
    fn run_a_day(scheduler: &mut Scheduler, day: u8, paused: bool) -> std::vec::Vec<(u16, u8)> {
        let mut fed = std::vec::Vec::new();
        for second in 0..DAY_S {
            let now = Wall {
                date: date(2026, 9, day),
                second_of_day: second,
                offset_minutes: None,
            };
            if let Due::Feed {
                minute_of_day,
                portions,
            } = scheduler.next_due(now, paused)
            {
                fed.push((minute_of_day, portions));
            }
        }
        fed
    }

    #[test]
    fn five_meals_a_day_all_fire_exactly_once() {
        // The schedule is allowed up to MAX_SLOTS entries and the usual case is
        // two, but three, four or five are perfectly ordinary. Resolving only
        // the latest outstanding slot per call must not cost any of them.
        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(
            Schedule::parse(
                br#"[{"time":"07:00","portions":1},
                     {"time":"10:00","portions":2},
                     {"time":"13:00","portions":1},
                     {"time":"16:00","portions":3},
                     {"time":"19:00","portions":2}]"#,
            )
            .unwrap(),
        );

        assert_eq!(
            run_a_day(&mut scheduler, 14, false),
            [(420, 1), (600, 2), (780, 1), (960, 3), (1140, 2)]
        );

        // And the next day does the same, rather than the marker from day one
        // suppressing anything.
        assert_eq!(run_a_day(&mut scheduler, 15, false).len(), 5);
    }

    #[test]
    fn a_full_schedule_is_fed_in_full() {
        // MAX_SLOTS is the documented limit, so walk right up to it.
        let mut payload = std::string::String::from("[");
        for slot in 0..MAX_SLOTS {
            if slot > 0 {
                payload.push(',');
            }
            payload.push_str(&std::format!(
                r#"{{"time":"{:02}:30","portions":1}}"#,
                slot + 8
            ));
        }
        payload.push(']');

        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(Schedule::parse(payload.as_bytes()).unwrap());

        assert_eq!(run_a_day(&mut scheduler, 14, false).len(), MAX_SLOTS);
    }

    #[test]
    fn slots_a_minute_apart_both_fire() {
        // Nothing in the guard assumes meals are hours apart.
        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(
            Schedule::parse(br#"[{"time":"08:00","portions":1},{"time":"08:01","portions":1}]"#)
                .unwrap(),
        );

        assert_eq!(run_a_day(&mut scheduler, 14, false), [(480, 1), (481, 1)]);
    }

    #[test]
    fn a_unit_that_boots_midday_still_feeds_the_rest_of_the_day() {
        // The baseline consumes everything already past, and must consume
        // nothing else. Three meals gone, two still to come.
        let mut scheduler = Scheduler::new();
        scheduler.set_schedule(
            Schedule::parse(
                br#"[{"time":"07:00","portions":1},
                     {"time":"10:00","portions":2},
                     {"time":"13:00","portions":1},
                     {"time":"16:00","portions":3},
                     {"time":"19:00","portions":2}]"#,
            )
            .unwrap(),
        );

        assert_eq!(
            scheduler.next_due(wall(14, 13, 30), false),
            Due::Consumed {
                minute_of_day: 780,
                why: Skipped::Baseline
            }
        );

        let mut fed = std::vec::Vec::new();
        for second in (13 * 3600 + 1801)..DAY_S {
            let now = Wall {
                date: date(2026, 9, 14),
                second_of_day: second,
                offset_minutes: None,
            };
            if let Due::Feed { minute_of_day, .. } = scheduler.next_due(now, false) {
                fed.push(minute_of_day);
            }
        }
        assert_eq!(fed, [960, 1140]);
    }

    #[test]
    fn only_the_latest_outstanding_slot_is_resolved_per_call() {
        // Two slots crossed at once means one missed meal and one resolution,
        // never two feeds queued back to back.
        let mut scheduler = two_meals();
        armed_at(&mut scheduler, wall(14, 7, 0));

        assert_eq!(
            scheduler.next_due(wall(14, 19, 0), false),
            Due::Feed {
                minute_of_day: 1140,
                portions: 3
            }
        );
        assert_eq!(scheduler.next_due(wall(14, 19, 1), false), Due::Nothing);
    }

    // --- the schedule in flash and on the echo topic -------------------------

    fn stored(json: &[u8]) -> Schedule {
        Schedule::parse(json).unwrap()
    }

    #[test]
    fn a_schedule_round_trips_through_flash() {
        for json in [
            &br#"[]"#[..],
            br#"[{"time":"08:00","portions":2},{"time":"19:00","portions":3}]"#,
            br#"[{"time":"00:00","portions":1},{"time":"23:59","portions":16}]"#,
        ] {
            let schedule = stored(json);
            assert_eq!(Schedule::decode(&schedule.encode()), Ok(schedule));
        }
    }

    #[test]
    fn a_full_schedule_round_trips_through_flash() {
        let mut json = alloc::string::String::from("[");
        for i in 0..MAX_SLOTS {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&alloc::format!(
                r#"{{"time":"{:02}:30","portions":{}}}"#,
                i * 3,
                i + 1
            ));
        }
        json.push(']');
        let schedule = stored(json.as_bytes());
        assert_eq!(schedule.len(), MAX_SLOTS);
        assert_eq!(Schedule::decode(&schedule.encode()), Ok(schedule));
    }

    /// Erased flash is a unit that has never been given a schedule, not a
    /// corrupt one — the ordinary state of a new unit.
    #[test]
    fn erased_flash_is_no_schedule() {
        assert_eq!(
            Schedule::decode(&[0xFF; SCHEDULE_RECORD_LEN]),
            Err(ScheduleRecordError::NotStored)
        );
    }

    /// Every single-bit flip is caught, as the credentials record's are.
    #[test]
    fn a_damaged_record_is_refused() {
        let good = stored(br#"[{"time":"08:00","portions":2}]"#).encode();
        for byte in 0..SCHEDULE_RECORD_LEN {
            for bit in 0..8 {
                let mut bad = good;
                bad[byte] ^= 1 << bit;
                assert!(
                    Schedule::decode(&bad).is_err(),
                    "flip at {byte}.{bit} accepted"
                );
            }
        }
    }

    #[test]
    fn a_record_cut_short_is_refused() {
        let good = stored(br#"[{"time":"08:00","portions":2}]"#).encode();
        assert_eq!(
            Schedule::decode(&good[..SCHEDULE_RECORD_LEN - 1]),
            Err(ScheduleRecordError::Corrupt)
        );
    }

    #[test]
    fn the_echo_is_the_json_the_commands_carry() {
        let schedule = stored(br#"[{"time":"08:00","portions":2},{"time":"19:05","portions":12}]"#);
        let json = schedule.to_json();
        assert_eq!(
            json.as_str(),
            r#"[{"time":"08:00","portions":2},{"time":"19:05","portions":12}]"#
        );
        assert_eq!(Schedule::parse(json.as_bytes()), Ok(schedule));
        assert_eq!(Schedule::new().to_json().as_str(), "[]");
    }

    /// The echo buffer holds the largest schedule the firmware accepts.
    #[test]
    fn the_largest_schedule_fits_its_echo() {
        let mut json = alloc::string::String::from("[");
        for i in 0..MAX_SLOTS {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&alloc::format!(
                r#"{{"time":"{:02}:59","portions":255}}"#,
                23 - i
            ));
        }
        json.push(']');
        let schedule = stored(json.as_bytes());
        assert_eq!(Schedule::parse(schedule.to_json().as_bytes()), Ok(schedule));
    }
}
