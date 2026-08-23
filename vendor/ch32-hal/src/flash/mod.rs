//! Flash memory (FLASH)
//! CH32 FLASH is relatively close to STM32. This module is heavily inspired from embassy-stm32
use embedded_storage::nor_flash::{NorFlashError, NorFlashErrorKind};

mod common;
pub use common::*;

const APPLICATION_FLASH: &crate::pac::mem_layout::MemoryRegion =
    match crate::pac::MEMORY_LAYOUT.find_by_role(crate::pac::mem_layout::MemoryRole::Application) {
        Some(region) => region,
        None => panic!("selected chip has no application flash region"),
    };

/// Physical application-flash address used for erase/program/read operations.
pub const FLASH_BASE: usize = APPLICATION_FLASH.address as usize;
/// Application-flash capacity in bytes.
pub const FLASH_SIZE: usize = APPLICATION_FLASH.size as usize;
/// Native page or standard write size in bytes.
pub const WRITE_SIZE: usize = match APPLICATION_FLASH.page_program_size() {
    Some(size) => size as usize,
    None => match APPLICATION_FLASH.write_size() {
        Some(size) => size as usize,
        None => panic!("selected chip has no supported flash-write mode"),
    },
};

/// Read size (always 1)
pub const READ_SIZE: usize = 1;

/// Blocking flash mode typestate.
pub enum Blocking {}
/// Async flash mode typestate.
pub enum Async {}

/// Flash sector.
#[derive(Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct FlashSector {
    /// Absolute start address.
    pub start: u32,
    /// Size in bytes.
    pub size: u32,
}

#[cfg_attr(flash_v3, path = "v3.rs")]
#[cfg_attr(not(flash_v3), path = "other.rs")]
mod family;

#[allow(unused_imports)]
pub use family::*;

/// Flash error
///
/// See CH32 Reference Manual for your chip for details.
#[allow(missing_docs)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    Prog,
    Size,
    Miss,
    Seq,
    Protected,
    Unaligned,
    Parallelism,
}

impl NorFlashError for Error {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Size => NorFlashErrorKind::OutOfBounds,
            Self::Unaligned => NorFlashErrorKind::NotAligned,
            _ => NorFlashErrorKind::Other,
        }
    }
}
