//! Source telemetry exposed independently of the PD wire implementation.

use crate::{Milliamps, Millivolts};

/// PPS regulator operating mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PpsOperatingMode {
    /// The Source is regulating the requested voltage.
    ConstantVoltage,
    /// The Source is regulating at the requested current limit.
    CurrentLimit,
}

impl PpsOperatingMode {
    /// Whether the Source reports that its current-limit loop is active.
    pub const fn is_current_limited(self) -> bool {
        matches!(self, Self::CurrentLimit)
    }
}

/// Temperature state used by PPS_Status and general Status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemperatureStatus {
    Unsupported,
    Normal,
    Warning,
    OverTemperature,
}

impl TemperatureStatus {
    const fn from_two_bits(bits: u8) -> Self {
        match bits & 0x03 {
            0 => Self::Unsupported,
            1 => Self::Normal,
            2 => Self::Warning,
            _ => Self::OverTemperature,
        }
    }
}

/// Source's reported internal temperature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InternalTemperature {
    Unsupported,
    BelowTwoCelsius,
    Celsius(u8),
}

/// External input currently powering the Source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalPowerInput {
    Internal,
    Dc,
    Ac,
    Invalid,
}

/// Requested system power state reported by general Status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerState {
    Unsupported,
    S0,
    ModernStandby,
    S3,
    S4,
    S5,
    G3,
    Invalid,
}

/// Requested user-visible indicator for a power-state change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerIndicator {
    Off,
    On,
    Blinking,
    Breathing,
    Invalid,
}

/// Live four-byte Programmable Power Supply status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PpsStatus {
    raw: [u8; 4],
}

impl PpsStatus {
    /// Construct from the four bytes defined by the PPSSDB.
    pub const fn from_raw_bytes(raw: [u8; 4]) -> Self {
        Self { raw }
    }

    /// Exact PPSSDB bytes, useful for diagnostics and transport adapters.
    pub const fn raw_bytes(self) -> [u8; 4] {
        self.raw
    }

    /// Source-side measured output voltage. Accuracy is specified as ±3%.
    pub const fn output_voltage(self) -> Option<Millivolts> {
        let units = u16::from_le_bytes([self.raw[0], self.raw[1]]);
        if units == u16::MAX {
            None
        } else {
            Some(Millivolts(units as u32 * 20))
        }
    }

    /// Source-side measured output current. Accuracy is specified as ±150 mA.
    pub const fn output_current(self) -> Option<Milliamps> {
        if self.raw[2] == u8::MAX {
            None
        } else {
            Some(Milliamps(self.raw[2] as u32 * 50))
        }
    }

    /// Present temperature state.
    pub const fn temperature(self) -> TemperatureStatus {
        TemperatureStatus::from_two_bits(self.raw[3] >> 1)
    }

    /// Whether the Source is regulating voltage or limiting current.
    pub const fn operating_mode(self) -> PpsOperatingMode {
        if self.raw[3] & (1 << 3) == 0 {
            PpsOperatingMode::ConstantVoltage
        } else {
            PpsOperatingMode::CurrentLimit
        }
    }

    /// Convenience hook for an application-controlled CL indicator LED.
    pub const fn is_current_limited(self) -> bool {
        self.operating_mode().is_current_limited()
    }
}

/// General seven-byte Status response from the Source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceStatus {
    raw: [u8; 7],
    pps_mode_valid: bool,
}

impl SourceStatus {
    /// Construct from the seven SDB bytes and whether the current contract is
    /// PPS, which determines whether the CV/CL flag is meaningful.
    pub const fn from_raw_bytes(raw: [u8; 7], pps_mode_valid: bool) -> Self {
        Self { raw, pps_mode_valid }
    }

    /// Exact SDB bytes, useful for diagnostics and transport adapters.
    pub const fn raw_bytes(self) -> [u8; 7] {
        self.raw
    }

    /// Whether the CV/CL bit is meaningful for the current contract.
    pub const fn pps_mode_valid(self) -> bool {
        self.pps_mode_valid
    }

    pub const fn internal_temperature(self) -> InternalTemperature {
        match self.raw[0] {
            0 => InternalTemperature::Unsupported,
            1 => InternalTemperature::BelowTwoCelsius,
            value => InternalTemperature::Celsius(value),
        }
    }

    pub const fn external_power_input(self) -> ExternalPowerInput {
        match (self.raw[1] >> 1) & 0x03 {
            0 => ExternalPowerInput::Internal,
            1 => ExternalPowerInput::Dc,
            2 => ExternalPowerInput::Invalid,
            _ => ExternalPowerInput::Ac,
        }
    }

    pub const fn powered_by_battery(self) -> bool {
        self.raw[1] & (1 << 3) != 0
    }

    pub const fn powered_by_non_battery(self) -> bool {
        self.raw[1] & (1 << 4) != 0
    }

    pub const fn present_battery_inputs(self) -> u8 {
        self.raw[2]
    }

    pub const fn overcurrent_event(self) -> bool {
        self.raw[3] & (1 << 1) != 0
    }

    pub const fn overtemperature_event(self) -> bool {
        self.raw[3] & (1 << 2) != 0
    }

    pub const fn overvoltage_event(self) -> bool {
        self.raw[3] & (1 << 3) != 0
    }

    /// CV/CL mode, or `None` when the active contract is not PPS.
    pub const fn pps_operating_mode(self) -> Option<PpsOperatingMode> {
        if !self.pps_mode_valid {
            None
        } else if self.raw[3] & (1 << 4) == 0 {
            Some(PpsOperatingMode::ConstantVoltage)
        } else {
            Some(PpsOperatingMode::CurrentLimit)
        }
    }

    pub const fn is_current_limited(self) -> bool {
        matches!(self.pps_operating_mode(), Some(PpsOperatingMode::CurrentLimit))
    }

    pub const fn temperature(self) -> TemperatureStatus {
        TemperatureStatus::from_two_bits(self.raw[4] >> 1)
    }

    pub const fn power_limited_by_cable(self) -> bool {
        self.raw[5] & (1 << 1) != 0
    }

    pub const fn power_limited_by_other_ports(self) -> bool {
        self.raw[5] & (1 << 2) != 0
    }

    pub const fn power_limited_by_external_power(self) -> bool {
        self.raw[5] & (1 << 3) != 0
    }

    pub const fn power_limited_by_event(self) -> bool {
        self.raw[5] & (1 << 4) != 0
    }

    pub const fn power_limited_by_temperature(self) -> bool {
        self.raw[5] & (1 << 5) != 0
    }

    pub const fn is_power_limited(self) -> bool {
        self.raw[5] & 0x3e != 0
    }

    pub const fn power_state(self) -> PowerState {
        match self.raw[6] & 0x07 {
            0 => PowerState::Unsupported,
            1 => PowerState::S0,
            2 => PowerState::ModernStandby,
            3 => PowerState::S3,
            4 => PowerState::S4,
            5 => PowerState::S5,
            6 => PowerState::G3,
            _ => PowerState::Invalid,
        }
    }

    pub const fn power_indicator(self) -> PowerIndicator {
        match (self.raw[6] >> 3) & 0x07 {
            0 => PowerIndicator::Off,
            1 => PowerIndicator::On,
            2 => PowerIndicator::Blinking,
            3 => PowerIndicator::Breathing,
            _ => PowerIndicator::Invalid,
        }
    }
}

/// Asynchronous status-change notification from the Source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceAlert {
    raw: u32,
}

impl SourceAlert {
    /// Construct from the Alert Data Object.
    pub const fn from_raw(raw: u32) -> Self {
        Self { raw }
    }

    pub const fn raw(self) -> u32 {
        self.raw
    }

    pub const fn overvoltage_event(self) -> bool {
        self.raw & (1 << 30) != 0
    }

    pub const fn source_input_changed(self) -> bool {
        self.raw & (1 << 29) != 0
    }

    /// Includes temperature-state and PPS CV/CL transitions.
    pub const fn operating_condition_changed(self) -> bool {
        self.raw & (1 << 28) != 0
    }

    pub const fn overtemperature_event(self) -> bool {
        self.raw & (1 << 27) != 0
    }

    pub const fn overcurrent_event(self) -> bool {
        self.raw & (1 << 26) != 0
    }

    pub const fn source_reducing_capabilities(self) -> bool {
        self.raw & (1 << 31) != 0 && self.raw & 0x0f == 5
    }
}

/// Which optional source inquiry failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StatusQuery {
    General,
    Pps,
    SourceInfo,
}

/// Non-fatal outcome of an optional status inquiry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StatusQueryFailure {
    NotSupported,
    Rejected,
    Deferred,
    Timeout,
    /// The query is not defined by the negotiated PD revision and was not transmitted.
    UnsupportedRevision,
}
