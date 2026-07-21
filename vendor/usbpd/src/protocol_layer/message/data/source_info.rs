//! Source information data objects.
//!
//! See USB PD R3.2 Table 6.29 and Table 6.30.

use byteorder::{ByteOrder, LittleEndian};
use proc_bitfield::bitfield;

bitfield! {
    /// Source_Info Data Object 1.
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct SourceInfoDataObject1(pub u32): Debug, FromStorage, IntoStorage {
        /// False for a managed-capability port, true for a guaranteed-capability port.
        pub guaranteed_capability_port: bool @ 31,
        /// Maximum power the port will provide, in whole watts.
        pub port_maximum_pdp_watts: u8 @ 16..=23,
        /// Power the port is presently capable of supplying, in whole watts.
        pub port_present_pdp_watts: u8 @ 8..=15,
        /// Power represented by the current Source Capabilities, in whole watts.
        pub port_reported_pdp_watts: u8 @ 0..=7,
    }
}

bitfield! {
    /// Source_Info Data Object 2.
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct SourceInfoDataObject2(pub u32): Debug, FromStorage, IntoStorage {
        /// False for a managed-capability port, true for a guaranteed-capability port.
        pub guaranteed_capability_port: bool @ 31,
        /// Whether the port behaves as a Dynamic Power Source.
        pub dynamic_power_source: bool @ 30,
        /// Maximum PDP in 0.5 W steps.
        pub raw_port_maximum_pdp_half_watts: u16 @ 9..=17,
        /// Guaranteed PDP in 0.5 W steps.
        pub raw_port_guaranteed_pdp_half_watts: u16 @ 0..=8,
    }
}

/// Parsed Source_Info message. PD 3.1 sources may provide only the first data
/// object; PD 3.2 defines the second data object as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SourceInfo {
    /// Dynamic and presently available power information.
    pub object1: SourceInfoDataObject1,
    /// Guaranteed and maximum capability details when supplied.
    pub object2: Option<SourceInfoDataObject2>,
}

impl SourceInfo {
    /// Parse one or two Source_Info data objects.
    pub fn from_bytes(payload: &[u8], num_objects: usize) -> Option<Self> {
        if !(1..=2).contains(&num_objects) || payload.len() < num_objects * core::mem::size_of::<u32>() {
            return None;
        }

        let object1 = SourceInfoDataObject1(LittleEndian::read_u32(&payload[..4]));
        let object2 = (num_objects == 2).then(|| SourceInfoDataObject2(LittleEndian::read_u32(&payload[4..8])));
        Some(Self { object1, object2 })
    }

    /// Serialize the represented data objects.
    pub fn to_bytes(self, payload: &mut [u8]) -> usize {
        LittleEndian::write_u32(payload, self.object1.0);
        if let Some(object2) = self.object2 {
            LittleEndian::write_u32(&mut payload[4..], object2.0);
            8
        } else {
            4
        }
    }

    /// Present power available from the source, in whole watts.
    pub fn port_present_pdp_watts(self) -> u8 {
        self.object1.port_present_pdp_watts()
    }
}
