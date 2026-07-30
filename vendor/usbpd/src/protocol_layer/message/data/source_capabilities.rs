//! Definitions of source capabilities data message content.
use heapless::Vec;
use proc_bitfield::bitfield;

use super::PdoKind;
use crate::units::{ElectricCurrent, ElectricPotential, Power};

/// Kinds of supplies that can be reported within source capabilities.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Kind {
    /// Fixed voltage supply.
    FixedSupply,
    /// Battery supply.
    Battery,
    /// Variable voltage supply.
    VariableSupply,
    /// Programmable power supply.
    Pps,
    /// Augmented voltage source.
    Avs,
}

/// A power data object holds information about one type of source capability.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PowerDataObject {
    /// Fixed voltage supply.
    FixedSupply(FixedSupply),
    /// Battery supply.
    Battery(Battery),
    /// Variable voltage supply.
    VariableSupply(VariableSupply),
    /// Augmented supply.
    Augmented(Augmented),
    /// Unknown kind of power data object.
    Unknown(RawPowerDataObject),
}

impl PowerDataObject {
    /// Check if this PDO is zero-padding (used in EPR capabilities messages).
    ///
    /// Per USB PD Spec R3.2 Section 6.5.15.1, if the SPR Capabilities Message
    /// contains fewer than 7 PDOs, the unused Data Objects are zero-filled.
    pub fn is_zero_padding(&self) -> bool {
        self.to_raw() == 0
    }

    /// Convert the PDO to its raw u32 representation.
    pub fn to_raw(&self) -> u32 {
        match self {
            PowerDataObject::FixedSupply(f) => f.0,
            PowerDataObject::Battery(b) => b.0,
            PowerDataObject::VariableSupply(v) => v.0,
            PowerDataObject::Augmented(a) => match a {
                Augmented::Spr(s) => s.0,
                Augmented::SprAvs(a) => a.0,
                Augmented::Epr(e) => e.0,
                Augmented::Unknown(u) => *u,
            },
            PowerDataObject::Unknown(u) => u.0,
        }
    }
}

bitfield! {
    /// A raw power data object.
    ///
    /// Used as a fallback for encoding unknown source types.
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct RawPowerDataObject(pub u32): Debug, FromStorage, IntoStorage {
        /// The kind of power data object.
        pub kind: u8 @ 30..=31,
    }
}

bitfield! {
    /// A fixed voltage supply PDO.
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct FixedSupply(pub u32): Debug, FromStorage, IntoStorage {
        /// Fixed supply
        pub kind: u8 @ 30..=31,
        /// Dual-role power
        pub dual_role_power: bool @ 29,
        /// USB suspend supported
        pub usb_suspend_supported: bool @ 28,
        /// Unconstrained power
        pub unconstrained_power: bool @ 27,
        /// USB communications capable
        pub usb_communications_capable: bool @ 26,
        /// Dual-role data
        pub dual_role_data: bool @ 25,
        /// Unchunked extended messages supported
        pub unchunked_extended_messages_supported: bool @ 24,
        /// EPR mode capable
        pub epr_mode_capable: bool @ 23,
        /// Peak current
        pub peak_current: u8 @ 20..=21,
        /// Voltage in 50 mV units
        pub raw_voltage: u16 @ 10..=19,
        /// Maximum current in 10 mA units
        pub raw_max_current: u16 @ 0..=9,
    }
}

#[allow(clippy::derivable_impls)]
impl Default for FixedSupply {
    fn default() -> Self {
        Self(0)
    }
}

impl FixedSupply {
    /// Voltage of the Fixed Supply
    pub fn voltage(&self) -> ElectricPotential {
        ElectricPotential::from_50mv_units(self.raw_voltage())
    }

    /// Maximum current of the Fixed Supply
    pub fn max_current(&self) -> ElectricCurrent {
        ElectricCurrent::from_10ma_units(self.raw_max_current())
    }

    /// Create a new Fixed Supply at vSafe5V with the rated current
    pub fn v_safe_5v(max_current_10ma: u16) -> Self {
        FixedSupply::default()
            .with_raw_voltage(100) // V = 5v = 100_u16 * 50mv
            .with_raw_max_current(max_current_10ma)
            .with_peak_current(0)
    }
}

bitfield! {
    /// A battery supply PDO.
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Battery(pub u32): Debug, FromStorage, IntoStorage {
        /// Battery
        pub kind: u8 @ 30..=31,
        /// Maximum Voltage in 50 mV units
        pub raw_max_voltage: u16 @ 20..=29,
        /// Minimum Voltage in 50 mV units
        pub raw_min_voltage: u16 @ 10..=19,
        /// Maximum Allowable Power in 250 mW units
        pub raw_max_power: u16 @ 0..=9,
    }
}

impl Battery {
    /// The maximum voltage the battery can supply
    pub fn max_voltage(&self) -> ElectricPotential {
        ElectricPotential::from_50mv_units(self.raw_max_voltage())
    }

    /// The minimum voltage the battery can supply
    pub fn min_voltage(&self) -> ElectricPotential {
        ElectricPotential::from_50mv_units(self.raw_min_voltage())
    }

    /// The maximum power the battery can supply
    pub fn max_power(&self) -> Power {
        Power::from_250mw_units(self.raw_max_power())
    }
}

bitfield! {
    /// A variable supply PDO.
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct VariableSupply(pub u32): Debug, FromStorage, IntoStorage {
        /// Variable supply (non-battery)
        pub kind: u8 @ 30..=31,
        /// Maximum Voltage in 50 mV units
        pub raw_max_voltage: u16 @ 20..=29,
        /// Minimum Voltage in 50 mV units
        pub raw_min_voltage: u16 @ 10..=19,
        /// Maximum current in 10 mA units
        pub raw_max_current: u16 @ 0..=9,
    }
}

impl VariableSupply {
    /// The maximum voltage the variable supply is capable of
    pub fn max_voltage(&self) -> ElectricPotential {
        ElectricPotential::from_50mv_units(self.raw_max_voltage())
    }

    /// The minimum voltage the variable supply is capable of
    pub fn min_voltage(&self) -> ElectricPotential {
        ElectricPotential::from_50mv_units(self.raw_min_voltage())
    }

    /// The maximum current the variable supply can offer
    pub fn max_current(&self) -> ElectricCurrent {
        ElectricCurrent::from_10ma_units(self.raw_max_current())
    }
}

/// An augmented PDO.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Augmented {
    /// SPR PPS
    Spr(SprProgrammablePowerSupply),
    /// SPR AVS
    SprAvs(SprAdjustableVoltageSupply),
    /// EPR AVS
    Epr(EprAdjustableVoltageSupply),
    /// Unknown
    Unknown(u32),
}

bitfield! {
    /// Augmented power data object, see PD Spec `6.4.1.2.4`
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct AugmentedRaw(pub u32): Debug, FromStorage, IntoStorage {
        /// Augmented power data object
        pub kind: u8 @ 30..=31,
        /// Supply type
        pub supply: u8 @ 28..=29,
        /// Field for augmented capabilities
        pub power_capabilities: u32 @ 0..=27,
    }
}

bitfield! {
    /// SPR PPS
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct SprProgrammablePowerSupply(pub u32): Debug, FromStorage, IntoStorage {
        /// Augmented power data object
        pub kind: u8 @ 30..=31,
        /// SPR programmable power supply
        pub supply: u8 @ 28..=29,
        /// Whether or not the PPS will limit the current to meet a certain power limit
        pub pps_power_limited: bool @ 27,
        /// Maximum voltage in 100 mV increments
        pub raw_max_voltage: u8 @ 17..=24,
        /// Minimum Voltage in 100 mV increments
        pub raw_min_voltage: u8 @ 8..=15,
        /// Maximum Current in 50 mA increments
        pub raw_max_current: u8 @ 0..=6,
    }
}

impl Default for SprProgrammablePowerSupply {
    fn default() -> Self {
        Self(0).with_kind(0b11).with_supply(0b00)
    }
}

impl SprProgrammablePowerSupply {
    /// The maximum voltage the PPS can be requested to supply
    pub fn max_voltage(&self) -> ElectricPotential {
        ElectricPotential::from_100mv_units(self.raw_max_voltage().into())
    }

    /// The minimum voltage the PPS can be requested to supply
    pub fn min_voltage(&self) -> ElectricPotential {
        ElectricPotential::from_100mv_units(self.raw_min_voltage().into())
    }

    /// The maximum current the PPS can supply.
    pub fn max_current(&self) -> ElectricCurrent {
        ElectricCurrent::from_50ma_units(self.raw_max_current().into())
    }
}

bitfield! {
    /// SPR Adjustable Voltage Supply APDO (USB PD 3.2 Table 6.15).
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct SprAdjustableVoltageSupply(pub u32): Debug, FromStorage, IntoStorage {
        /// Augmented power data object
        pub kind: u8 @ 30..=31,
        /// SPR AVS
        pub supply: u8 @ 28..=29,
        /// Peak current capability during the Overload Period
        pub peak_current: u8 @ 26..=27,
        /// Maximum current at 15 V in 10 mA increments
        pub raw_max_current_15v: u16 @ 10..=19,
        /// Maximum current at 20 V in 10 mA increments; zero means 15 V maximum
        pub raw_max_current_20v: u16 @ 0..=9,
    }
}

impl SprAdjustableVoltageSupply {
    /// Minimum requestable SPR AVS voltage, fixed by the PDO format at 9 V.
    pub fn min_voltage(&self) -> ElectricPotential {
        ElectricPotential::from_volts(9)
    }

    /// Maximum requestable voltage: 15 V when the 20 V current field is zero,
    /// otherwise 20 V.
    pub fn max_voltage(&self) -> ElectricPotential {
        ElectricPotential::from_volts(if self.raw_max_current_20v() == 0 { 15 } else { 20 })
    }

    /// Advertised current at a voltage inside the APDO's 9-15/20 V range.
    pub fn max_current_at(&self, voltage: ElectricPotential) -> Option<ElectricCurrent> {
        if voltage < self.min_voltage() || voltage > self.max_voltage() {
            None
        } else if voltage <= ElectricPotential::from_volts(15) {
            Some(ElectricCurrent::from_10ma_units(self.raw_max_current_15v()))
        } else {
            Some(ElectricCurrent::from_10ma_units(self.raw_max_current_20v()))
        }
    }
}

bitfield! {
    /// EPR AVS PDO
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct EprAdjustableVoltageSupply(pub u32): Debug, FromStorage, IntoStorage {
        /// Augmented power data object
        pub kind: u8 @ 30..=31,
        /// EPR adjustable voltage supply
        pub supply: u8 @ 28..=29,
        /// Peak current capability during the Overload Period
        pub peak_current: u8 @ 26..=27,
        /// Maximum voltage in 100 mV increments
        pub raw_max_voltage: u16 @ 17..=25,
        /// Minimum Voltage in 100 mV increments
        pub raw_min_voltage: u8 @ 8..=15,
        /// PDP in 1 W increments
        pub raw_pd_power: u8 @ 0..=7,
    }
}

impl EprAdjustableVoltageSupply {
    /// The maximum voltage the PPS can be requested to supply
    pub fn max_voltage(&self) -> ElectricPotential {
        ElectricPotential::from_100mv_units(self.raw_max_voltage())
    }

    /// The minimum voltage the PPS can be requested to supply
    pub fn min_voltage(&self) -> ElectricPotential {
        ElectricPotential::from_100mv_units(self.raw_min_voltage().into())
    }

    /// Rated power the PPS can supply
    pub fn pd_power(&self) -> Power {
        Power::from_watts(self.raw_pd_power())
    }
}

const MAX_CAPABILITIES_LEN: usize = 16;
/// Maximum number of PDOs in an EPR Source Capabilities message: seven SPR
/// positions followed by at most four EPR positions.
pub const MAX_EPR_SOURCE_PDOS: usize = 11;

/// List of `PDOs` that the `Source` lists as capabilities
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SourceCapabilities(pub(crate) Vec<PowerDataObject, MAX_CAPABILITIES_LEN>);

impl SourceCapabilities {
    /// Create a new `SourceCapabilities` from a `Vec` of PDOs
    pub fn new_with_pdos(pdos: Vec<PowerDataObject, MAX_CAPABILITIES_LEN>) -> Self {
        Self(pdos)
    }

    /// Create a new `SourceCapabilities` with only the required
    /// `vSafe5V` fixed PDO in the first position.
    pub fn new_vsafe5v_only(maximum_current_10ma: u16) -> Self {
        let mut inner = Vec::new();
        inner.push(PowerDataObject::FixedSupply(FixedSupply::v_safe_5v(maximum_current_10ma))).ok();
        Self(inner)
    }

    /// Get the fixed-supply PDO in the required first position. This accessor
    /// does not validate its voltage/current fields; use
    /// [`Self::has_valid_vsafe_5v`] when accepting partner capabilities.
    pub fn vsafe_5v(&self) -> Option<&FixedSupply> {
        self.0.first().and_then(
            |supply| {
                if let PowerDataObject::FixedSupply(supply) = supply { Some(supply) } else { None }
            },
        )
    }

    /// Check that the first PDO is the mandatory fixed 5 V offer with a
    /// non-zero current no greater than the 5 A Type-C limit.
    pub fn has_valid_vsafe_5v(&self) -> bool {
        self.vsafe_5v()
            .is_some_and(|supply| supply.raw_voltage() == 100 && (1..=500).contains(&supply.raw_max_current()))
    }

    /// Determine, whether or not this device is `dual role`.
    pub fn dual_role_power(&self) -> bool {
        self.vsafe_5v().map(FixedSupply::dual_role_power).unwrap_or_default()
    }

    /// Determine, whether or not USB suspends are supported by the source.
    pub fn usb_suspend_supported(&self) -> bool {
        self.vsafe_5v().map(FixedSupply::usb_suspend_supported).unwrap_or_default()
    }

    /// Determine, whether or not this device constrains the power to a certain level.
    pub fn unconstrained_power(&self) -> bool {
        self.vsafe_5v().map(FixedSupply::unconstrained_power).unwrap_or_default()
    }

    /// Determine, whether dual-role data is supported by the source.
    pub fn dual_role_data(&self) -> bool {
        self.vsafe_5v().map(FixedSupply::dual_role_data).unwrap_or_default()
    }

    /// Determine, whether unchunked extended messages are supported by the source.
    pub fn unchunked_extended_messages_supported(&self) -> bool {
        self.vsafe_5v().map(FixedSupply::unchunked_extended_messages_supported).unwrap_or_default()
    }

    /// Determine, whether the source is EPR mode capable.
    pub fn epr_mode_capable(&self) -> bool {
        self.vsafe_5v().map(FixedSupply::epr_mode_capable).unwrap_or_default()
    }

    /// Get power data objects (PDOs) from the source.
    pub fn pdos(&self) -> &[PowerDataObject] {
        &self.0
    }

    /// Check if this is an EPR capabilities message (has PDOs at position 8+).
    ///
    /// Per USB PD Spec R3.2 Section 6.5.15.1, EPR Capabilities Messages have
    /// SPR PDOs in positions 1-7 and EPR PDOs starting at position 8.
    pub fn is_epr_capabilities(&self) -> bool {
        self.0.len() > 7
    }

    /// Validate the structural length of an EPR Source Capabilities message.
    pub fn has_valid_epr_length(&self) -> bool {
        (8..=MAX_EPR_SOURCE_PDOS).contains(&self.0.len())
    }

    /// Get SPR PDOs (positions 1-7), excluding zero-padding entries.
    ///
    /// Per USB PD Spec R3.2 Section 6.5.15.1:
    /// - Positions 1-7 contain SPR (A)PDOs
    /// - If fewer than 7 SPR PDOs exist, unused positions are zero-filled
    ///
    /// Returns iterator of (position, PDO) tuples where position is 1-indexed.
    pub fn spr_pdos(&self) -> impl Iterator<Item = (u8, &PowerDataObject)> {
        self.0.iter().take(7).enumerate().filter(|(_, pdo)| !pdo.is_zero_padding()).map(|(i, pdo)| ((i + 1) as u8, pdo))
    }

    /// Get EPR PDOs (positions 8+).
    ///
    /// Per USB PD Spec R3.2 Section 6.5.15.1:
    /// - EPR (A)PDOs start at Data Object position 8
    /// - Only valid in EPR Capabilities Messages
    ///
    /// Returns iterator of (position, PDO) tuples where position is 1-indexed (8, 9, 10, 11).
    pub fn epr_pdos(&self) -> impl Iterator<Item = (u8, &PowerDataObject)> {
        self.0.iter().skip(7).enumerate().map(|(i, pdo)| ((i + 8) as u8, pdo))
    }

    /// Check if any EPR PDO is in invalid position (1-7).
    ///
    /// Per USB PD Spec R3.2 Section 8.3.3.3.8:
    /// "In EPR Mode and An EPR_Source_Capabilities Message is received with
    /// an EPR (A)PDO in object positions 1... 7" → Hard Reset
    ///
    /// EPR (A)PDOs per spec:
    /// - Fixed Supply PDOs offering 28V, 36V, or 48V (voltage > 20V)
    /// - EPR AVS APDOs
    pub fn has_epr_pdo_in_spr_positions(&self) -> bool {
        let max_spr_voltage = ElectricPotential::from_volts(20);
        self.0.iter().take(7).any(|pdo| match pdo {
            // EPR Fixed Supply: voltage > 20V
            PowerDataObject::FixedSupply(f) => f.voltage() > max_spr_voltage,
            // EPR AVS APDO
            PowerDataObject::Augmented(Augmented::Epr(_)) => true,
            _ => false,
        })
    }

    /// Convert to bytes for transmission.
    ///
    /// Each PDO is 4 bytes, little-endian.
    pub fn to_bytes(&self, buffer: &mut [u8]) -> usize {
        let mut offset = 0;
        for pdo in &self.0 {
            let raw = pdo.to_raw();
            buffer[offset..offset + 4].copy_from_slice(&raw.to_le_bytes());
            offset += 4;
        }
        offset
    }
}

impl PdoKind for SourceCapabilities {
    fn at_object_position(&self, position: u8) -> Option<Kind> {
        self.pdos().get(position.saturating_sub(1) as usize).and_then(|pdo| match pdo {
            PowerDataObject::FixedSupply(_) => Some(Kind::FixedSupply),
            PowerDataObject::Battery(_) => Some(Kind::Battery),
            PowerDataObject::VariableSupply(_) => Some(Kind::VariableSupply),
            PowerDataObject::Augmented(augmented) => match augmented {
                Augmented::Spr(_) => Some(Kind::Pps),
                Augmented::SprAvs(_) => Some(Kind::Avs),
                Augmented::Epr(_) => Some(Kind::Avs),
                Augmented::Unknown(_) => None,
            },
            PowerDataObject::Unknown(_) => None,
        })
    }
}

impl PdoKind for Option<SourceCapabilities> {
    fn at_object_position(&self, position: u8) -> Option<Kind> {
        self.as_ref().at_object_position(position)
    }
}

impl PdoKind for Option<&SourceCapabilities> {
    fn at_object_position(&self, position: u8) -> Option<Kind> {
        self.and_then(|s| s.at_object_position(position))
    }
}

/// Parse a raw PDO into a typed power data object.
///
/// Decodes the PDO type bits and constructs the appropriate variant.
/// Supports SPR (Fixed, Battery, Variable, PPS) and EPR (AVS) PDO types.
pub fn parse_raw_pdo(raw: u32) -> PowerDataObject {
    let pdo = RawPowerDataObject(raw);
    match pdo.kind() {
        0b00 => PowerDataObject::FixedSupply(FixedSupply(raw)),
        0b01 => PowerDataObject::Battery(Battery(raw)),
        0b10 => PowerDataObject::VariableSupply(VariableSupply(raw)),
        0b11 => PowerDataObject::Augmented(match AugmentedRaw(raw).supply() {
            0b00 => Augmented::Spr(SprProgrammablePowerSupply(raw)),
            0b01 => Augmented::Epr(EprAdjustableVoltageSupply(raw)),
            0b10 => Augmented::SprAvs(SprAdjustableVoltageSupply(raw)),
            x => {
                warn!("Unknown AugmentedPowerDataObject supply {}", x);
                Augmented::Unknown(raw)
            }
        }),
        _ => {
            warn!("Unknown PowerDataObject kind");
            PowerDataObject::Unknown(pdo)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spr_avs(current_15v_10ma: u32, current_20v_10ma: u32) -> u32 {
        (0b11 << 30) | (0b10 << 28) | (current_15v_10ma << 10) | current_20v_10ma
    }

    #[test]
    fn spr_avs_is_typed_and_exposes_its_real_range() {
        let raw = spr_avs(400, 300);
        let PowerDataObject::Augmented(Augmented::SprAvs(avs)) = parse_raw_pdo(raw) else {
            panic!("SPR AVS must not be reduced to an unknown APDO")
        };

        assert_eq!(avs.0, raw);
        assert_eq!(avs.min_voltage(), ElectricPotential::from_volts(9));
        assert_eq!(avs.max_voltage(), ElectricPotential::from_volts(20));
        assert_eq!(avs.max_current_at(ElectricPotential::from_volts(15)), Some(ElectricCurrent::from_milliamps(4_000)));
        assert_eq!(avs.max_current_at(ElectricPotential::from_volts(20)), Some(ElectricCurrent::from_milliamps(3_000)));
        assert_eq!(avs.max_current_at(ElectricPotential::from_volts(8)), None);

        let PowerDataObject::Augmented(Augmented::SprAvs(fifteen_volt_only)) = parse_raw_pdo(spr_avs(400, 0)) else {
            panic!("SPR AVS must stay typed")
        };
        assert_eq!(fifteen_volt_only.max_voltage(), ElectricPotential::from_volts(15));
    }

    #[test]
    fn captured_pdos_decode_to_the_same_integer_quantities() {
        let PowerDataObject::FixedSupply(fixed) = parse_raw_pdo(0x001f_01f4) else {
            panic!("captured PDO10 must be a fixed supply")
        };
        assert_eq!(fixed.voltage().as_millivolts(), 48_000);
        assert_eq!(fixed.max_current().as_milliamps(), 5_000);

        let PowerDataObject::Augmented(Augmented::Spr(pps)) = parse_raw_pdo(0xc9a4_3264) else {
            panic!("captured PDO6 must be PPS")
        };
        assert_eq!(pps.min_voltage().as_millivolts(), 5_000);
        assert_eq!(pps.max_voltage().as_millivolts(), 21_000);
        assert_eq!(pps.max_current().as_milliamps(), 5_000);

        let PowerDataObject::Augmented(Augmented::Epr(avs)) = parse_raw_pdo(0xd7c0_96f0) else {
            panic!("captured PDO11 must be EPR AVS")
        };
        assert_eq!(avs.min_voltage().as_millivolts(), 15_000);
        assert_eq!(avs.max_voltage().as_millivolts(), 48_000);
        assert_eq!(avs.pd_power().as_milliwatts(), 240_000);
    }
}
