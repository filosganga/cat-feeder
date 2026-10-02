//! Checking a firmware image while it arrives (ADR-0022).
//!
//! An upload is written to the idle app slot as it streams in, so nothing here
//! sees the image whole. What it decides, before the slot is ever selected:
//!
//! - **It is an ESP image** of a size that fits the slot: the first byte is
//!   [`MAGIC`].
//! - **It arrived intact.** `espflash save-image` ends the image with the
//!   SHA-256 of everything before it; a truncated or corrupted transfer fails
//!   here rather than at the bootloader.
//! - **It carries this build's `ap_secret`.** Every unit's setup and admin
//!   password derives from it, so an image built from another `cfg.toml` would
//!   change them on the next boot — and the admin page that took the upload
//!   would no longer let anyone in. The image must contain [`secret_mark`] of
//!   this unit's secret; one from before this check has no mark at all and is
//!   refused the same way, which sends it over USB.
//!
//! Hardware-free, so the whole decision is host-tested; `firmware.rs` does the
//! flash writes.

use crate::sha256::Sha256;

/// The first byte of every ESP application image.
pub const MAGIC: u8 = 0xE9;

/// The trailing SHA-256 `espflash save-image` appends.
pub const DIGEST_LEN: usize = 32;

/// Smaller than any image this firmware could be: a header and a digest.
pub const MIN_IMAGE_LEN: usize = 1024;

/// What a mark starts with. Only ever compiled into [`secret_mark`]'s output,
/// so the image holds it once, directly before the secret.
pub const MARK_PREFIX: [u8; 8] = *b"cf-secr:";

/// The longest secret a mark is built from.
pub const MAX_SECRET_LEN: usize = 64;

/// The longest mark: the prefix and the longest secret.
pub const MAX_MARK_LEN: usize = MARK_PREFIX.len() + MAX_SECRET_LEN;

/// The bytes an image must contain to prove it was built with `secret`.
///
/// `N` is `MARK_PREFIX.len() + secret.len()`; any other value fails to
/// compile when the mark is a `static`.
pub const fn secret_mark<const N: usize>(secret: &str) -> [u8; N] {
    let secret = secret.as_bytes();
    assert!(N == MARK_PREFIX.len() + secret.len(), "mark length");
    assert!(secret.len() <= MAX_SECRET_LEN, "secret too long");
    let mut mark = [0u8; N];
    let mut i = 0;
    while i < MARK_PREFIX.len() {
        mark[i] = MARK_PREFIX[i];
        i += 1;
    }
    let mut j = 0;
    while j < secret.len() {
        mark[MARK_PREFIX.len() + j] = secret[j];
        j += 1;
    }
    mark
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    /// Shorter than [`MIN_IMAGE_LEN`].
    TooSmall,
    /// Longer than the app slot.
    TooLarge,
    /// Does not start with [`MAGIC`].
    NotAnImage,
    /// More bytes arrived than `Content-Length` promised.
    Overrun,
    /// The connection ended before `Content-Length` bytes.
    Truncated,
    /// The trailing digest does not match the bytes before it.
    Corrupt,
    /// No mark of this unit's `ap_secret`.
    WrongSecret,
}

impl ImageError {
    /// For the admin page's answer.
    pub const fn message(self) -> &'static str {
        match self {
            Self::TooSmall => "Too small to be a firmware image.",
            Self::TooLarge => "Larger than the firmware slot.",
            Self::NotAnImage => "Not an ESP32 firmware image (make it with espflash save-image).",
            Self::Overrun => "More data arrived than the request declared.",
            Self::Truncated => "The upload ended early; nothing was changed.",
            Self::Corrupt => "The image failed its checksum; nothing was changed.",
            Self::WrongSecret => {
                "Built with a different ap_secret, or before OTA support; flash it over USB."
            }
        }
    }
}

/// An app slot, as far as [`awaiting_confirmation`] needs one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppSlot {
    Ota0,
    Ota1,
    /// A factory or test slot, which this partition table has not.
    Other,
}

/// An `otadata` entry's state, mirroring esp-bootloader-esp-idf's
/// `OtaImageState` so the decision below is host-tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    New,
    PendingVerify,
    Valid,
    Invalid,
    Aborted,
    Undefined,
}

/// Whether the bootloader is waiting for the running image to prove itself
/// (ADR-0024): it gave a `New` image its one boot and marked it
/// `PendingVerify`, and resets until it is confirmed go back to the previous
/// slot.
///
/// Only when `otadata` selects the slot that actually booted. After a
/// rollback it still selects the failed image, marked `Aborted`, while the old
/// one runs — and confirming that would be confirming the wrong slot.
pub fn awaiting_confirmation(booted: AppSlot, selected: AppSlot, state: Option<SlotState>) -> bool {
    booted == selected && state == Some(SlotState::PendingVerify)
}

/// The running verdict on one upload.
pub struct ImageCheck<'m> {
    expected: usize,
    seen: usize,
    hasher: Sha256,
    digest: [u8; DIGEST_LEN],
    mark: Matcher<'m>,
}

impl<'m> ImageCheck<'m> {
    /// `len` is the request's `Content-Length`, `slot_len` the app slot's size.
    pub fn new(len: usize, slot_len: usize, mark: &'m [u8]) -> Result<Self, ImageError> {
        if len < MIN_IMAGE_LEN {
            return Err(ImageError::TooSmall);
        }
        if len > slot_len {
            return Err(ImageError::TooLarge);
        }
        Ok(Self {
            expected: len,
            seen: 0,
            hasher: Sha256::new(),
            digest: [0; DIGEST_LEN],
            mark: Matcher::new(mark),
        })
    }

    /// The next bytes, in order. An error is final: stop writing.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), ImageError> {
        if bytes.is_empty() {
            return Ok(());
        }
        if self.seen == 0 && bytes[0] != MAGIC {
            return Err(ImageError::NotAnImage);
        }
        let end = self.seen + bytes.len();
        if end > self.expected {
            return Err(ImageError::Overrun);
        }

        // Everything before the last DIGEST_LEN bytes is hashed; those are kept.
        let hashed_end = self.expected - DIGEST_LEN;
        let split = hashed_end.saturating_sub(self.seen).min(bytes.len());
        self.hasher.update(&bytes[..split]);
        for (i, byte) in bytes[split..].iter().enumerate() {
            self.digest[self.seen + split + i - hashed_end] = *byte;
        }

        self.mark.feed(bytes);
        self.seen = end;
        Ok(())
    }

    /// After the last byte. `Ok` means the image may be selected.
    pub fn finish(self) -> Result<(), ImageError> {
        if self.seen != self.expected {
            return Err(ImageError::Truncated);
        }
        if self.hasher.finish() != self.digest {
            return Err(ImageError::Corrupt);
        }
        if !self.mark.found() {
            return Err(ImageError::WrongSecret);
        }
        Ok(())
    }
}

/// Finds a needle in a stream fed in arbitrary pieces (Knuth–Morris–Pratt, so
/// a partial match is never lost at a piece boundary or by a false start).
struct Matcher<'m> {
    needle: &'m [u8],
    fail: [u8; MAX_MARK_LEN],
    matched: usize,
    found: bool,
}

impl<'m> Matcher<'m> {
    fn new(needle: &'m [u8]) -> Self {
        let needle = &needle[..needle.len().min(MAX_MARK_LEN)];
        let mut fail = [0u8; MAX_MARK_LEN];
        let mut k = 0;
        for i in 1..needle.len() {
            while k > 0 && needle[i] != needle[k] {
                k = fail[k - 1] as usize;
            }
            if needle[i] == needle[k] {
                k += 1;
            }
            fail[i] = k as u8;
        }
        Self {
            needle,
            fail,
            matched: 0,
            found: needle.is_empty(),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        if self.found {
            return;
        }
        for &byte in bytes {
            while self.matched > 0 && byte != self.needle[self.matched] {
                self.matched = self.fail[self.matched - 1] as usize;
            }
            if byte == self.needle[self.matched] {
                self.matched += 1;
                if self.matched == self.needle.len() {
                    self.found = true;
                    return;
                }
            }
        }
    }

    fn found(&self) -> bool {
        self.found
    }
}

/// Collects a stream into whole flash sectors, so each is erased and written
/// once. `SECTOR` is the flash's erase size.
pub struct Sectors<const SECTOR: usize> {
    buffer: [u8; SECTOR],
    filled: usize,
    /// Where the buffer's first byte goes, from the start of the slot.
    at: u32,
}

impl<const SECTOR: usize> Default for Sectors<SECTOR> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SECTOR: usize> Sectors<SECTOR> {
    pub const fn new() -> Self {
        Self {
            buffer: [0xFF; SECTOR],
            filled: 0,
            at: 0,
        }
    }

    /// Back to the start of the slot. The buffer is a `static` shared by every
    /// upload, so one that failed half way must not leave its position behind.
    pub fn reset(&mut self) {
        self.filled = 0;
        self.at = 0;
    }

    /// Takes as much of `bytes` as fits; returns how much it took and, when
    /// that filled the buffer, the sector to write and where.
    pub fn push(&mut self, bytes: &[u8]) -> (usize, Option<(u32, &[u8])>) {
        let take = bytes.len().min(SECTOR - self.filled);
        self.buffer[self.filled..self.filled + take].copy_from_slice(&bytes[..take]);
        self.filled += take;
        if self.filled < SECTOR {
            return (take, None);
        }
        let at = self.at;
        self.at += SECTOR as u32;
        self.filled = 0;
        (take, Some((at, &self.buffer[..])))
    }

    /// The last, partial sector, padded with erased bytes to a 4-byte
    /// boundary (the flash's write size), or `None` if nothing is left.
    pub fn rest(&mut self) -> Option<(u32, &[u8])> {
        if self.filled == 0 {
            return None;
        }
        let len = self.filled.next_multiple_of(4);
        self.buffer[self.filled..len].fill(0xFF);
        let at = self.at;
        self.at += SECTOR as u32;
        self.filled = 0;
        Some((at, &self.buffer[..len]))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;
    use crate::sha256::sha256;

    const SECRET: &str = "0123456789abcdef";
    const MARK: [u8; 24] = secret_mark::<24>(SECRET);
    /// An app slot's size in `partitions.csv`; only a test fixture here, the
    /// unit reads the real one from its partition table.
    const SLOT: usize = 0x1E_0000;

    /// An image as `espflash save-image` makes one: magic, body with the mark
    /// somewhere inside, then the digest of all of it.
    fn image(len: usize, mark: &[u8], mark_at: usize) -> Vec<u8> {
        let mut body: Vec<u8> = (0..len - DIGEST_LEN)
            .map(|i| (i * 31 % 251) as u8)
            .collect();
        body[0] = MAGIC;
        body[mark_at..mark_at + mark.len()].copy_from_slice(mark);
        let digest = sha256(&body);
        body.extend_from_slice(&digest);
        body
    }

    fn check(bytes: &[u8], declared: usize, piece: usize) -> Result<(), ImageError> {
        let mut check = ImageCheck::new(declared, SLOT, &MARK)?;
        for chunk in bytes.chunks(piece) {
            check.feed(chunk)?;
        }
        check.finish()
    }

    #[test]
    fn a_new_image_on_its_first_boot_awaits_confirmation() {
        use AppSlot::*;
        assert!(awaiting_confirmation(
            Ota1,
            Ota1,
            Some(SlotState::PendingVerify)
        ));
        assert!(awaiting_confirmation(
            Ota0,
            Ota0,
            Some(SlotState::PendingVerify)
        ));
    }

    #[test]
    fn a_confirmed_or_usb_flashed_image_does_not() {
        use AppSlot::*;
        assert!(!awaiting_confirmation(Ota0, Ota0, Some(SlotState::Valid)));
        assert!(!awaiting_confirmation(Ota0, Ota0, None));
        // Without a rollback bootloader the state stays New: nothing waits.
        assert!(!awaiting_confirmation(Ota1, Ota1, Some(SlotState::New)));
    }

    #[test]
    fn after_a_rollback_the_failed_slot_is_not_confirmed() {
        // otadata still selects the image that failed; the old one runs.
        use AppSlot::*;
        assert!(!awaiting_confirmation(Ota1, Ota0, Some(SlotState::Aborted)));
        assert!(!awaiting_confirmation(
            Ota1,
            Ota0,
            Some(SlotState::PendingVerify)
        ));
    }

    #[test]
    fn the_mark_is_the_prefix_then_the_secret() {
        assert_eq!(&MARK[..8], b"cf-secr:");
        assert_eq!(&MARK[8..], SECRET.as_bytes());
    }

    #[test]
    fn a_good_image_passes_in_any_piece_size() {
        let img = image(4096 + 77, &MARK, 1000);
        for piece in [1, 2, 3, 31, 32, 33, 536, 1460, 4096, img.len()] {
            assert_eq!(check(&img, img.len(), piece), Ok(()), "pieces of {piece}");
        }
    }

    #[test]
    fn a_mark_split_across_pieces_is_found() {
        let img = image(3000, &MARK, 1460 - 5);
        assert_eq!(check(&img, img.len(), 1460), Ok(()));
    }

    #[test]
    fn a_mark_at_the_very_end_of_the_body_is_found() {
        let len = 2048;
        let img = image(len, &MARK, len - DIGEST_LEN - MARK.len());
        assert_eq!(check(&img, len, 100), Ok(()));
    }

    #[test]
    fn another_secret_is_refused() {
        let other = secret_mark::<24>("fedcba9876543210");
        let img = image(4096, &other, 500);
        assert_eq!(check(&img, img.len(), 512), Err(ImageError::WrongSecret));
    }

    #[test]
    fn no_mark_at_all_is_refused() {
        let img = image(4096, &[], 0);
        assert_eq!(check(&img, img.len(), 512), Err(ImageError::WrongSecret));
    }

    #[test]
    fn a_near_miss_before_the_mark_does_not_hide_it() {
        // The prefix repeated with a wrong byte right before the real mark:
        // a matcher that restarts from scratch on a mismatch loses the mark.
        let mut near = Vec::from(&MARK[..]);
        near[10] ^= 1;
        let mut both = near.clone();
        both.extend_from_slice(&MARK);
        let img = image(4096, &both, 300);
        assert_eq!(check(&img, img.len(), 7), Ok(()));
    }

    #[test]
    fn a_flipped_byte_is_corrupt() {
        let mut img = image(4096, &MARK, 100);
        img[2000] ^= 0x40;
        assert_eq!(check(&img, img.len(), 512), Err(ImageError::Corrupt));
    }

    #[test]
    fn a_flipped_digest_byte_is_corrupt() {
        let mut img = image(4096, &MARK, 100);
        let last = img.len() - 1;
        img[last] ^= 1;
        assert_eq!(check(&img, img.len(), 512), Err(ImageError::Corrupt));
    }

    #[test]
    fn a_short_upload_is_truncated() {
        let img = image(4096, &MARK, 100);
        assert_eq!(
            check(&img[..4000], img.len(), 512),
            Err(ImageError::Truncated)
        );
    }

    #[test]
    fn more_than_declared_is_an_overrun() {
        let img = image(4096, &MARK, 100);
        assert_eq!(check(&img, img.len() - 4, 512), Err(ImageError::Overrun));
    }

    #[test]
    fn the_first_byte_must_be_the_magic() {
        let mut img = image(4096, &MARK, 100);
        img[0] = 0x7F;
        assert_eq!(check(&img, img.len(), 512), Err(ImageError::NotAnImage));
    }

    #[test]
    fn sizes_outside_the_slot_are_refused_up_front() {
        assert_eq!(
            ImageCheck::new(MIN_IMAGE_LEN - 1, SLOT, &MARK).err(),
            Some(ImageError::TooSmall)
        );
        assert_eq!(
            ImageCheck::new(SLOT + 1, SLOT, &MARK).err(),
            Some(ImageError::TooLarge)
        );
        assert!(ImageCheck::new(SLOT, SLOT, &MARK).is_ok());
    }

    #[test]
    fn sectors_reassemble_the_stream_padded_to_a_word() {
        let data: Vec<u8> = (0..10_003u32).map(|i| (i % 253) as u8).collect();
        for piece in [1, 7, 1460, 4096, 5000] {
            let mut sectors = Sectors::<4096>::new();
            let mut out = std::vec![0xFFu8; 3 * 4096];
            let mut written = Vec::new();
            for chunk in data.chunks(piece) {
                let mut rest = chunk;
                while !rest.is_empty() {
                    let (took, full) = sectors.push(rest);
                    if let Some((at, sector)) = full {
                        out[at as usize..at as usize + sector.len()].copy_from_slice(sector);
                        written.push((at, sector.len()));
                    }
                    rest = &rest[took..];
                }
            }
            let (at, sector) = sectors.rest().expect("a partial sector");
            out[at as usize..at as usize + sector.len()].copy_from_slice(sector);
            written.push((at, sector.len()));
            assert_eq!(
                written,
                [(0, 4096), (4096, 4096), (8192, 1812)],
                "pieces of {piece}"
            );
            assert_eq!(&out[..data.len()], &data[..]);
            assert_eq!(&out[data.len()..10_004], &[0xFF]);
            assert!(sectors.rest().is_none());
        }
    }

    #[test]
    fn reset_starts_again_at_the_slot_start() {
        let mut sectors = Sectors::<16>::new();
        sectors.push(&[1; 20]);
        sectors.reset();
        let (_, full) = sectors.push(&[2; 16]);
        assert_eq!(full.map(|(at, _)| at), Some(0));
        assert!(sectors.rest().is_none());
    }
}
