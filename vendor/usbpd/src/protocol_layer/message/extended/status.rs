//! SOP Status Data Block (SDB).

/// Seven-byte general Status Data Block returned by a Port Partner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Status {
    bytes: [u8; Self::DATA_SIZE],
}

impl Status {
    /// Size of the defined SOP Status Data Block.
    pub const DATA_SIZE: usize = 7;

    /// Parse the required fields. Per the Extended Message rules, any future
    /// trailing fields are ignored.
    pub fn from_bytes(payload: &[u8]) -> Option<Self> {
        let bytes = payload.get(..Self::DATA_SIZE)?.try_into().ok()?;
        Some(Self { bytes })
    }

    /// Serialize the currently defined fields.
    pub fn to_bytes(self, payload: &mut [u8]) -> usize {
        payload[..Self::DATA_SIZE].copy_from_slice(&self.bytes);
        Self::DATA_SIZE
    }

    /// Raw defined bytes.
    pub const fn raw_bytes(self) -> [u8; Self::DATA_SIZE] {
        self.bytes
    }

    /// Internal-temperature byte.
    pub const fn internal_temperature_raw(self) -> u8 {
        self.bytes[0]
    }

    /// Present-input flags.
    pub const fn present_input_raw(self) -> u8 {
        self.bytes[1]
    }

    /// Present-battery-input bitmap.
    pub const fn present_battery_input_raw(self) -> u8 {
        self.bytes[2]
    }

    /// Event flags, including OCP/OTP/OVP and PPS CV/CL state.
    pub const fn event_flags(self) -> u8 {
        self.bytes[3]
    }

    /// Temperature-status flags.
    pub const fn temperature_status_raw(self) -> u8 {
        self.bytes[4]
    }

    /// Power-limiting reason flags.
    pub const fn power_status(self) -> u8 {
        self.bytes[5]
    }

    /// Power-state and indicator field.
    pub const fn power_state_change(self) -> u8 {
        self.bytes[6]
    }
}
