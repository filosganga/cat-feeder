//! The provisioning record in flash.
//!
//! A thin wrapper: everything about the record's *shape* is in
//! [`crate::provisioning`] and host-tested. This part only moves bytes to and
//! from the `nvs` partition, which cannot be tested anywhere but on hardware.
//!
//! ## Which partition, and why it is free
//!
//! `partitions.csv` gives `nvs` 24 KB at 0x9000, as the default table does, so
//! units flashed before the OTA slots (ADR-0022) keep their records. Nothing
//! else in this firmware uses it. Two things confirm that: esp-radio's `NVS`
//! symbol is a 15-word array in RAM inside its ESP-IDF shim, not the partition
//! (`misc_nvs_restore` is a `todo!()`), and it sets `nvs_enable: 0` in the
//! init config it hands the blob.
//!
//! The partition is found through the table rather than hardcoded, so a unit
//! flashed with a different layout still works or fails loudly.
//!
//! **Configuration survives a reflash.** `espflash` rewrites the app partition
//! and leaves this one alone, which is what makes `cargo run` bearable once a
//! unit is set up: it keeps its credentials across every rebuild.
//!
//! ## Four records, four sectors
//!
//! | Offset in `nvs` | Record | Written by |
//! |---|---|---|
//! | `0x0000` | credentials and calibration, `FDR2` | `provision.sh`, setup mode, the knob's settings, the admin page, both reset gestures (`Record::without_network`) |
//! | `0x1000` | the schedule, `FDS1` | a schedule command, a `Meal n` entity, the admin page |
//! | `0x2000` | the timezone, `FDZ1` | the admin page |
//! | `0x3000` | the pause, `FDP1` | the knob, the admin page, a live `feeder/<id>/paused` |
//!
//! A sector each, so writing one never rewrites another, and no format has to
//! change for another. It also means `provision.sh`, which erases
//! only the first sector before writing, keeps a unit's meals and pause across
//! a re-provision — as does the boot-button reset, which is for re-entering
//! Wi-Fi details, not for forgetting the cats' schedule. The menu's factory
//! reset erases all four.

use embedded_storage::{ReadStorage, Storage};
use esp_bootloader_esp_idf::partitions::{
    self, DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType,
};
use esp_hal::peripherals::FLASH;
use esp_storage::FlashStorage;

use crate::provisioning::{DecodeError, MAX_RECORD_LEN, Record};
use crate::schedule::{
    PAUSE_RECORD_LEN, SCHEDULE_RECORD_LEN, Schedule, ScheduleRecordError, decode_paused,
    encode_paused,
};
use crate::tz::{ZONE_RECORD_LEN, Zone, ZoneRecordError};

/// Where the timezone record starts: the sector after the schedule's.
const ZONE_OFFSET: u32 = 0x2000;

/// Where the pause record starts: the sector after the timezone's.
const PAUSE_OFFSET: u32 = 0x3000;

/// Where the schedule record starts, from the start of the partition. One
/// flash sector after the credentials.
const SCHEDULE_OFFSET: u32 = 0x1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// The partition table has no `nvs` entry, or could not be read at all.
    NoPartition,
    /// The flash itself refused a read, write or erase.
    Flash,
    /// There is no usable record here. [`DecodeError::NotConfigured`] is the
    /// ordinary case — a unit that has never been set up — and not a fault.
    Record(DecodeError),
}

/// The store behind a lock, for the two tasks that write through it: `ui`
/// for the knob's settings and reset, `schedule` for schedule commands.
pub type SharedStore =
    embassy_sync::mutex::Mutex<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, Store>;

/// Reads and writes the records.
pub struct Store {
    flash: FlashStorage<'static>,
    offset: u32,
    len: u32,
}

impl Store {
    /// Finds the `nvs` partition and keeps its bounds.
    ///
    /// The table is read once, here, because the buffer it needs is three
    /// kilobytes and a boot-time cost is worth not paying it on every access.
    pub fn new(flash: FLASH<'static>) -> Result<Self, StoreError> {
        // On the heap, not the stack and not a static. The partition table
        // needs three kilobytes, this runs once at boot, and the memory goes
        // back afterwards.
        //
        // A `[0u8; PARTITION_TABLE_MAX_LEN]` local would be three kilobytes of
        // stack in whatever task called this — and so would handing that array
        // to a `StaticCell`, because its initialiser takes the value by value
        // and builds it on the stack first. `deny(clippy::large_stack_frames)`
        // catches both.
        let mut table_buffer = alloc::vec![0u8; PARTITION_TABLE_MAX_LEN];

        let mut flash = FlashStorage::new(flash);

        let table = partitions::read_partition_table(&mut flash, &mut table_buffer)
            .map_err(|_| StoreError::NoPartition)?;
        let entry = table
            .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
            .map_err(|_| StoreError::NoPartition)?
            .ok_or(StoreError::NoPartition)?;

        let (offset, len) = (entry.offset(), entry.len());
        if (len as usize) < PAUSE_OFFSET as usize + PAUSE_RECORD_LEN {
            return Err(StoreError::NoPartition);
        }

        Ok(Self { flash, offset, len })
    }

    /// Where the record lives, for the log line that says so once at boot.
    pub fn location(&self) -> (u32, u32) {
        (self.offset, self.len)
    }

    /// The stored record, or why there isn't one.
    // Out of line: each holds a record-sized buffer, which inlined would
    // land in the async task that called it and stay there. See `main.rs`.
    #[inline(never)]
    pub fn load(&mut self) -> Result<Record, StoreError> {
        let mut buffer = [0u8; MAX_RECORD_LEN];
        self.flash
            .read(self.offset, &mut buffer)
            .map_err(|_| StoreError::Flash)?;

        Record::decode(&buffer).map_err(StoreError::Record)
    }

    /// Writes the record, replacing whatever was there.
    ///
    /// `Storage::write` reads the sector, patches it, erases and writes it
    /// back, so this is safe over an existing record — NOR flash can only clear
    /// bits, and writing without erasing would AND the two together.
    // Out of line: each holds a record-sized buffer, which inlined would
    // land in the async task that called it and stay there. See `main.rs`.
    #[inline(never)]
    pub fn save(&mut self, record: &Record) -> Result<(), StoreError> {
        let mut buffer = [0u8; MAX_RECORD_LEN];
        let len = record.encode(&mut buffer).map_err(|_| StoreError::Flash)?;

        self.flash
            .write(self.offset, &buffer[..len])
            .map_err(|_| StoreError::Flash)
    }

    /// Reads the record, lets `change` edit it, and writes it back.
    ///
    /// Everything `change` does not touch is written back exactly as it was
    /// stored, credentials included. The two record-sized copies live in this
    /// frame and are gone when it returns — which is why this is a method
    /// here rather than a load and a save in an async task's body, where
    /// they would be counted against that task's frame.
    #[inline(never)]
    pub fn update(&mut self, change: impl FnOnce(&mut Record)) -> Result<(), StoreError> {
        let mut record = self.load()?;
        change(&mut record);
        self.save(&record)
    }

    /// The stored schedule, or why there isn't one.
    #[inline(never)]
    pub fn load_schedule(&mut self) -> Result<Schedule, ScheduleRecordError> {
        let mut buffer = [0u8; SCHEDULE_RECORD_LEN];
        self.flash
            .read(self.offset + SCHEDULE_OFFSET, &mut buffer)
            .map_err(|_| ScheduleRecordError::Corrupt)?;
        Schedule::decode(&buffer)
    }

    /// Writes the schedule, replacing whatever was there. Same read-patch-
    /// erase-write as [`Store::save`], in the schedule's own sector.
    #[inline(never)]
    pub fn save_schedule(&mut self, schedule: &Schedule) -> Result<(), StoreError> {
        self.flash
            .write(self.offset + SCHEDULE_OFFSET, &schedule.encode())
            .map_err(|_| StoreError::Flash)
    }

    /// The stored timezone, or why there isn't one.
    #[inline(never)]
    pub fn load_zone(&mut self) -> Result<Zone, ZoneRecordError> {
        let mut buffer = [0u8; ZONE_RECORD_LEN];
        self.flash
            .read(self.offset + ZONE_OFFSET, &mut buffer)
            .map_err(|_| ZoneRecordError::Corrupt)?;
        Zone::decode(&buffer)
    }

    /// Writes the timezone, or erases it for `None`.
    #[inline(never)]
    pub fn save_zone(&mut self, zone: Option<&Zone>) -> Result<(), StoreError> {
        let bytes = zone.map_or([0xFFu8; ZONE_RECORD_LEN], Zone::encode);
        self.flash
            .write(self.offset + ZONE_OFFSET, &bytes)
            .map_err(|_| StoreError::Flash)
    }

    /// The stored pause, `None` for a sector with no pause record in it.
    #[inline(never)]
    pub fn load_paused(&mut self) -> Result<Option<bool>, StoreError> {
        let mut buffer = [0u8; PAUSE_RECORD_LEN];
        self.flash
            .read(self.offset + PAUSE_OFFSET, &mut buffer)
            .map_err(|_| StoreError::Flash)?;
        Ok(decode_paused(&buffer))
    }

    /// Writes the pause, replacing whatever was there.
    #[inline(never)]
    pub fn save_paused(&mut self, paused: bool) -> Result<(), StoreError> {
        self.flash
            .write(self.offset + PAUSE_OFFSET, &encode_paused(paused))
            .map_err(|_| StoreError::Flash)
    }

    /// Throws away every record: credentials, calibration, meals, the
    /// timezone and the pause. The menu's factory reset.
    #[inline(never)]
    pub fn erase_all(&mut self) -> Result<(), StoreError> {
        self.erase()?;
        let blank = [0xFFu8; SCHEDULE_RECORD_LEN];
        self.flash
            .write(self.offset + SCHEDULE_OFFSET, &blank)
            .map_err(|_| StoreError::Flash)?;
        self.save_zone(None)?;
        self.flash
            .write(self.offset + PAUSE_OFFSET, &[0xFFu8; PAUSE_RECORD_LEN])
            .map_err(|_| StoreError::Flash)
    }

    /// Forgets the network and keeps the calibration: the record rewritten by
    /// [`Record::without_network`], so the next boot goes to setup mode and
    /// setup mode finds the detent interval and portion scale still there.
    /// What both reset gestures do. A record that does not decode has nothing
    /// worth keeping, and is erased instead.
    #[inline(never)]
    pub fn forget_network(&mut self) -> Result<(), StoreError> {
        match self.load() {
            Ok(record) => self.save(&record.without_network()),
            Err(_) => self.erase(),
        }
    }

    /// Throws the credentials record away, so the next boot goes to setup.
    /// The schedule is kept — see the module docs.
    ///
    /// Writes the erased pattern rather than only clobbering the magic, so
    /// nothing recognisable is left behind — the old Wi-Fi password included.
    // Out of line: each holds a record-sized buffer, which inlined would
    // land in the async task that called it and stay there. See `main.rs`.
    #[inline(never)]
    pub fn erase(&mut self) -> Result<(), StoreError> {
        let blank = [0xFFu8; MAX_RECORD_LEN];
        self.flash
            .write(self.offset, &blank)
            .map_err(|_| StoreError::Flash)
    }
}
