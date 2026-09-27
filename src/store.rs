//! The provisioning record in flash.
//!
//! A thin wrapper: everything about the record's *shape* is in
//! [`crate::provisioning`] and host-tested. This part only moves bytes to and
//! from the `nvs` partition, which cannot be tested anywhere but on hardware.
//!
//! ## Which partition, and why it is free
//!
//! The default ESP-IDF partition table gives `nvs` 24 KB at 0x9000, and nothing
//! else in this firmware uses it. Two things confirm that: esp-radio's `NVS`
//! symbol is a 15-word array in RAM inside its ESP-IDF shim, not the partition
//! (`misc_nvs_restore` is a `todo!()`), and it sets `nvs_enable: 0` in the
//! init config it hands the blob. So no custom partition table is needed.
//!
//! The partition is found through the table rather than hardcoded, so a unit
//! flashed with a different layout still works or fails loudly.
//!
//! **Configuration survives a reflash.** `espflash` rewrites the app partition
//! and leaves this one alone, which is what makes `cargo run` bearable once a
//! unit is set up: it keeps its credentials across every rebuild.
//!
//! ## Two records, two sectors
//!
//! | Offset in `nvs` | Record | Written by |
//! |---|---|---|
//! | `0x0000` | credentials and calibration, `FDR2` | `provision.sh`, setup mode, the knob's settings |
//! | `0x1000` | the schedule, `FDS1` | a `feeder/<id>/schedule` or `feeder/all/schedule` command |
//!
//! A sector each, so writing one never rewrites the other, and neither format
//! has to change for the other. It also means `provision.sh`, which erases
//! only the first sector before writing, keeps a unit's meals across a
//! re-provision — as does the boot-button reset, which is for re-entering
//! Wi-Fi details, not for forgetting the cats' schedule. The menu's factory
//! reset erases both.

use embedded_storage::{ReadStorage, Storage};
use esp_bootloader_esp_idf::partitions::{
    self, DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType,
};
use esp_hal::peripherals::FLASH;
use esp_storage::FlashStorage;

use crate::provisioning::{DecodeError, MAX_RECORD_LEN, Record};
use crate::schedule::{SCHEDULE_RECORD_LEN, Schedule, ScheduleRecordError};

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
        if (len as usize) < SCHEDULE_OFFSET as usize + SCHEDULE_RECORD_LEN {
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

    /// Throws away both records: credentials, calibration and meals. The
    /// menu's factory reset.
    #[inline(never)]
    pub fn erase_all(&mut self) -> Result<(), StoreError> {
        self.erase()?;
        let blank = [0xFFu8; SCHEDULE_RECORD_LEN];
        self.flash
            .write(self.offset + SCHEDULE_OFFSET, &blank)
            .map_err(|_| StoreError::Flash)
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
