//! Alert Data Object (ADO).

use byteorder::{ByteOrder, LittleEndian};

/// A status-change notification sent by either Port Partner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AlertDataObject(pub u32);

impl AlertDataObject {
    /// Parse the single Alert Data Object.
    pub fn from_bytes(payload: &[u8]) -> Option<Self> {
        (payload.len() == 4).then(|| Self(LittleEndian::read_u32(payload)))
    }

    /// Serialize the Alert Data Object.
    pub fn to_bytes(self, payload: &mut [u8]) -> usize {
        LittleEndian::write_u32(payload, self.0);
        4
    }

    /// An extended alert event is present in bits 3..0.
    pub const fn extended_alert_event(self) -> bool {
        self.0 & (1 << 31) != 0
    }

    /// Overvoltage protection was triggered.
    pub const fn overvoltage_event(self) -> bool {
        self.0 & (1 << 30) != 0
    }

    /// The Source or Sink input changed.
    pub const fn source_input_change(self) -> bool {
        self.0 & (1 << 29) != 0
    }

    /// Temperature or PPS CV/CL operating condition changed.
    pub const fn operating_condition_change(self) -> bool {
        self.0 & (1 << 28) != 0
    }

    /// Overtemperature protection was triggered.
    pub const fn overtemperature_event(self) -> bool {
        self.0 & (1 << 27) != 0
    }

    /// Overcurrent protection was triggered.
    pub const fn overcurrent_event(self) -> bool {
        self.0 & (1 << 26) != 0
    }

    /// One or more battery states changed.
    pub const fn battery_status_change(self) -> bool {
        self.0 & (1 << 25) != 0
    }

    /// Extended alert event type in bits 3..0.
    pub const fn extended_alert_event_type(self) -> u8 {
        (self.0 & 0x0f) as u8
    }

    /// The Source reports that it is about to reduce its capabilities.
    pub const fn source_reducing_capabilities(self) -> bool {
        self.extended_alert_event() && self.extended_alert_event_type() == 5
    }

    /// Whether this alert contains a non-battery status change for which the
    /// Sink should issue `Get_Status`.
    pub const fn has_non_battery_status_change(self) -> bool {
        self.overvoltage_event()
            || self.source_input_change()
            || self.operating_condition_change()
            || self.overtemperature_event()
            || self.overcurrent_event()
            || self.extended_alert_event()
    }
}
