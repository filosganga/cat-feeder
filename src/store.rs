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

use embedded_storage::{ReadStorage, Storage};
use esp_bootloader_esp_idf::partitions::{
    self, DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType,
};
use esp_hal::peripherals::FLASH;
use esp_storage::FlashStorage;

use crate::provisioning::{DecodeError, MAX_RECORD_LEN, Record};

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

/// Reads and writes the one record.
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
        if (len as usize) < MAX_RECORD_LEN {
            return Err(StoreError::NoPartition);
        }

        Ok(Self { flash, offset, len })
    }

    /// Where the record lives, for the log line that says so once at boot.
    pub fn location(&self) -> (u32, u32) {
        (self.offset, self.len)
    }

    /// The stored record, or why there isn't one.
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
    pub fn save(&mut self, record: &Record) -> Result<(), StoreError> {
        let mut buffer = [0u8; MAX_RECORD_LEN];
        let len = record.encode(&mut buffer).map_err(|_| StoreError::Flash)?;

        self.flash
            .write(self.offset, &buffer[..len])
            .map_err(|_| StoreError::Flash)
    }

    /// Throws the record away, so the next boot goes to setup.
    ///
    /// Writes the erased pattern rather than only clobbering the magic, so
    /// nothing recognisable is left behind — the old Wi-Fi password included.
    pub fn erase(&mut self) -> Result<(), StoreError> {
        let blank = [0xFFu8; MAX_RECORD_LEN];
        self.flash
            .write(self.offset, &blank)
            .map_err(|_| StoreError::Flash)
    }
}
