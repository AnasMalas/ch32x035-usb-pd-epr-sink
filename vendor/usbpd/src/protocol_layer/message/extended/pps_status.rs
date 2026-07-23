//! Programmable Power Supply Status Data Block (PPSSDB).

use byteorder::{ByteOrder, LittleEndian};

/// PPS temperature state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TemperatureStatus {
    /// The Source does not report temperature status.
    Unsupported,
    /// Temperature is normal.
    Normal,
    /// The Source is warning that temperature is elevated.
    Warning,
    /// The Source reports overtemperature.
    OverTemperature,
}

/// PPS regulator operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OperatingMode {
    /// The Source is regulating output voltage.
    ConstantVoltage,
    /// The Source is regulating at the requested current limit.
    CurrentLimit,
}

/// Four-byte PPS Status Data Block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PpsStatus {
    output_voltage: u16,
    output_current: u8,
    flags: u8,
}

impl PpsStatus {
    /// Size of the defined PPSSDB. Later trailing fields are ignored.
    pub const DATA_SIZE: usize = 4;

    /// Parse the required fields. Per the Extended Message rules, any future
    /// trailing fields are ignored.
    pub fn from_bytes(payload: &[u8]) -> Option<Self> {
        (payload.len() >= Self::DATA_SIZE).then(|| Self {
            output_voltage: LittleEndian::read_u16(payload),
            output_current: payload[2],
            flags: payload[3],
        })
    }

    /// Serialize the currently defined fields.
    pub fn to_bytes(self, payload: &mut [u8]) -> usize {
        LittleEndian::write_u16(payload, self.output_voltage);
        payload[2] = self.output_current;
        payload[3] = self.flags;
        Self::DATA_SIZE
    }

    /// Raw output-voltage field in 20 mV units, or `None` when unsupported.
    pub const fn output_voltage_20mv(self) -> Option<u16> {
        if self.output_voltage == u16::MAX { None } else { Some(self.output_voltage) }
    }

    /// Raw output-current field in 50 mA units, or `None` when unsupported.
    pub const fn output_current_50ma(self) -> Option<u8> {
        if self.output_current == u8::MAX { None } else { Some(self.output_current) }
    }

    /// Present temperature state.
    pub const fn temperature_status(self) -> TemperatureStatus {
        match (self.flags >> 1) & 0x03 {
            0 => TemperatureStatus::Unsupported,
            1 => TemperatureStatus::Normal,
            2 => TemperatureStatus::Warning,
            _ => TemperatureStatus::OverTemperature,
        }
    }

    /// Whether the Source reports CV or CL operation.
    pub const fn operating_mode(self) -> OperatingMode {
        if self.flags & (1 << 3) == 0 { OperatingMode::ConstantVoltage } else { OperatingMode::CurrentLimit }
    }

    /// Raw defined bytes.
    pub const fn raw_bytes(self) -> [u8; Self::DATA_SIZE] {
        let voltage = self.output_voltage.to_le_bytes();
        [voltage[0], voltage[1], self.output_current, self.flags]
    }
}
