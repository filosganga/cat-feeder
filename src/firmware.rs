//! The two app slots and `otadata` (ADR-0022, ADR-0024): which image is
//! running, writing an upload into the other slot, selecting it, and
//! confirming the running one once it has proved itself.
//!
//! What an upload must be is decided in [`crate::update`], which is pure; this
//! is only the flash. Every call takes the [`Store`], because there is one
//! `FlashStorage` and the store owns it.
//!
//! The slot written is **the one not running**, read from the MMU, never
//! inferred from `otadata`: after a rollback `otadata` still names the image
//! that failed, and the crate's own "next slot" arithmetic picks from
//! `otadata`.

use alloc::vec;

use embedded_storage::nor_flash::NorFlash;
use esp_bootloader_esp_idf::ota::OtaImageState;
use esp_bootloader_esp_idf::ota_updater::OtaUpdater;
use esp_bootloader_esp_idf::partitions::{
    self, AppPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType,
};

use crate::store::Store;
use crate::update;

/// A flash sector, the unit an upload is erased and written in.
pub const SECTOR: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareError {
    /// The partition table could not be read, or has no `otadata` or fewer
    /// than two app slots: this unit was flashed before OTA (ADR-0022) and
    /// needs one USB flash.
    NoSlots,
    /// The booted image is not in an OTA slot.
    Unknown,
    /// `otadata` is blank. The bootloader initialises it on the first boot
    /// after a USB flash, so this is not expected; selecting from blank would
    /// hit the crate's arithmetic for a factory slot, which this table has not.
    Blank,
    /// The flash refused a read, write or erase.
    Flash,
}

impl FirmwareError {
    pub const fn message(self) -> &'static str {
        match self {
            Self::NoSlots => "This unit has no OTA slots yet; flash it once over USB.",
            Self::Unknown => "Cannot tell which firmware slot is running.",
            Self::Blank => "The OTA data is blank; reboot the unit and try again.",
            Self::Flash => "The flash refused a write; nothing was selected.",
        }
    }
}

/// Where an upload goes.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub slot: AppPartitionSubType,
    /// Absolute flash offset of the slot.
    pub offset: u32,
    pub len: u32,
}

/// The running image, as the boot log reports it.
#[derive(Debug, Clone, Copy)]
pub struct Running {
    pub booted: AppPartitionSubType,
    /// What `otadata` selects, which differs from `booted` after a rollback.
    pub selected: AppPartitionSubType,
    pub state: Option<OtaImageState>,
}

impl Running {
    /// The bootloader is waiting for this image to prove itself: reset before
    /// [`confirm`] and it boots the previous one. Decided in
    /// [`update::awaiting_confirmation`].
    pub fn pending(&self) -> bool {
        update::awaiting_confirmation(
            app_slot(self.booted),
            app_slot(self.selected),
            self.state.map(slot_state),
        )
    }
}

fn app_slot(slot: AppPartitionSubType) -> update::AppSlot {
    match slot {
        AppPartitionSubType::Ota0 => update::AppSlot::Ota0,
        AppPartitionSubType::Ota1 => update::AppSlot::Ota1,
        _ => update::AppSlot::Other,
    }
}

fn slot_state(state: OtaImageState) -> update::SlotState {
    match state {
        OtaImageState::New => update::SlotState::New,
        OtaImageState::PendingVerify => update::SlotState::PendingVerify,
        OtaImageState::Valid => update::SlotState::Valid,
        OtaImageState::Invalid => update::SlotState::Invalid,
        OtaImageState::Aborted => update::SlotState::Aborted,
        OtaImageState::Undefined => update::SlotState::Undefined,
    }
}

/// The partition table buffer: three kilobytes, so on the heap and only for
/// the length of a call. See `Store::new`.
fn table_buffer() -> alloc::vec::Vec<u8> {
    vec![0u8; PARTITION_TABLE_MAX_LEN]
}

fn as_table(buffer: &mut [u8]) -> &mut [u8; PARTITION_TABLE_MAX_LEN] {
    buffer.try_into().expect("PARTITION_TABLE_MAX_LEN bytes")
}

/// The booted slot, from the MMU.
fn booted(store: &mut Store) -> Result<AppPartitionSubType, FirmwareError> {
    let mut buffer = table_buffer();
    let table = partitions::read_partition_table(store.flash(), as_table(&mut buffer))
        .map_err(|_| FirmwareError::NoSlots)?;
    match table.booted_partition() {
        Ok(Some(entry)) => match entry.partition_type() {
            PartitionType::App(slot @ (AppPartitionSubType::Ota0 | AppPartitionSubType::Ota1)) => {
                Ok(slot)
            }
            _ => Err(FirmwareError::Unknown),
        },
        _ => Err(FirmwareError::Unknown),
    }
}

/// Which slot is running and what `otadata` says about it.
pub fn running(store: &mut Store) -> Result<Running, FirmwareError> {
    let booted = booted(store)?;
    let mut buffer = table_buffer();
    let mut updater = OtaUpdater::new(store.flash(), as_table(&mut buffer))
        .map_err(|_| FirmwareError::NoSlots)?;
    let selected = updater
        .selected_partition()
        .map_err(|_| FirmwareError::Flash)?;
    let state = updater.current_ota_state().ok();
    Ok(Running {
        booted,
        selected,
        state,
    })
}

/// The slot an upload may overwrite: the one not running.
pub fn idle_slot(store: &mut Store) -> Result<Target, FirmwareError> {
    let slot = match booted(store)? {
        AppPartitionSubType::Ota0 => AppPartitionSubType::Ota1,
        _ => AppPartitionSubType::Ota0,
    };
    let mut buffer = table_buffer();
    let table = partitions::read_partition_table(store.flash(), as_table(&mut buffer))
        .map_err(|_| FirmwareError::NoSlots)?;
    let entry = table
        .find_partition(PartitionType::App(slot))
        .map_err(|_| FirmwareError::NoSlots)?
        .ok_or(FirmwareError::NoSlots)?;
    Ok(Target {
        slot,
        offset: entry.offset(),
        len: entry.len(),
    })
}

/// Erases the sector at `at` in the target slot and writes `bytes` into it.
/// `at` is sector-aligned and `bytes` at most a sector, a multiple of four
/// bytes: what [`crate::update::Sectors`] hands out.
pub fn write_sector(
    store: &mut Store,
    target: Target,
    at: u32,
    bytes: &[u8],
) -> Result<(), FirmwareError> {
    if at as usize + SECTOR > target.len as usize || bytes.len() > SECTOR {
        return Err(FirmwareError::Flash);
    }
    let start = target.offset + at;
    let flash = store.flash();
    flash
        .erase(start, start + SECTOR as u32)
        .map_err(|_| FirmwareError::Flash)?;
    flash.write(start, bytes).map_err(|_| FirmwareError::Flash)
}

/// Selects the target slot for the next boot, as a `New` image the bootloader
/// gives one chance (ADR-0024).
///
/// The state is written explicitly: selecting a slot reuses whatever state the
/// `otadata` entry it lands in last held, which may be an old `Aborted`.
pub fn select(store: &mut Store, target: Target) -> Result<(), FirmwareError> {
    let mut buffer = table_buffer();
    let mut updater = OtaUpdater::new(store.flash(), as_table(&mut buffer))
        .map_err(|_| FirmwareError::NoSlots)?;
    let mut ota = updater.ota_data().map_err(|_| FirmwareError::NoSlots)?;
    if ota
        .current_app_partition()
        .map_err(|_| FirmwareError::Flash)?
        == AppPartitionSubType::Factory
    {
        return Err(FirmwareError::Blank);
    }
    ota.set_current_app_partition(target.slot)
        .map_err(|_| FirmwareError::Flash)?;
    ota.set_current_ota_state(OtaImageState::New)
        .map_err(|_| FirmwareError::Flash)
}

/// Marks the running image valid, if the bootloader is waiting on it.
/// `Ok(true)` means it was pending and now is not.
pub fn confirm(store: &mut Store) -> Result<bool, FirmwareError> {
    if !running(store)?.pending() {
        return Ok(false);
    }
    let mut buffer = table_buffer();
    let mut updater = OtaUpdater::new(store.flash(), as_table(&mut buffer))
        .map_err(|_| FirmwareError::NoSlots)?;
    updater
        .set_current_ota_state(OtaImageState::Valid)
        .map_err(|_| FirmwareError::Flash)?;
    Ok(true)
}
