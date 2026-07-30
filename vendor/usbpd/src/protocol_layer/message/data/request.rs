//! Definitions of request data message content.
use byteorder::{ByteOrder, LittleEndian};
use proc_bitfield::bitfield;

use super::source_capabilities;
use crate::units::{ElectricCurrent, ElectricPotential, Power};

bitfield! {
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct RawDataObject(pub u32): Debug, FromStorage, IntoStorage {
        /// Valid range 1..=14
        pub object_position: u8 @ 28..=31,
    }
}

bitfield! {
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct FixedVariableSupply(pub u32): Debug, FromStorage, IntoStorage {
        /// Valid range 1..=14
        pub object_position: u8 @ 28..=31,
        pub giveback_flag: bool @ 27,
        pub capability_mismatch: bool @ 26,
        pub usb_communications_capable: bool @ 25,
        pub no_usb_suspend: bool @ 24,
        /// Unchunked extended messages supported.
        /// WARNING: Do not set to true - the library always uses chunked mode
        /// for compatibility with more PHYs.
        pub unchunked_extended_messages_supported: bool @ 23,
        pub epr_mode_capable: bool @ 22,
        pub raw_operating_current: u16 @ 10..=19,
        pub raw_max_operating_current: u16 @ 0..=9,
    }
}

impl FixedVariableSupply {
    pub fn to_bytes(self, buf: &mut [u8]) -> usize {
        LittleEndian::write_u32(buf, self.0);
        4
    }

    pub fn operating_current(&self) -> ElectricCurrent {
        ElectricCurrent::from_10ma_units(self.raw_operating_current())
    }

    pub fn max_operating_current(&self) -> ElectricCurrent {
        ElectricCurrent::from_10ma_units(self.raw_max_operating_current())
    }
}

bitfield! {
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Battery(pub u32): Debug, FromStorage, IntoStorage {
        /// Object position (0000b and 1110b…1111b are Reserved and Shall Not be used)
        pub object_position: u8 @ 28..=31,
        /// GiveBackFlag = 0
        pub giveback_flag: bool @ 27,
        /// Capability mismatch
        pub capability_mismatch: bool @ 26,
        /// USB communications capable
        pub usb_communications_capable: bool @ 25,
        /// No USB Suspend
        pub no_usb_suspend: bool @ 24,
        /// Unchunked extended messages supported.
        /// WARNING: Do not set to true - the library always uses chunked mode
        /// for compatibility with more PHYs.
        pub unchunked_extended_messages_supported: bool @ 23,
        /// EPR mode capable
        pub epr_mode_capable: bool @ 22,
        /// Operating power in 250 mW units
        pub raw_operating_power: u16 @ 10..=19,
        /// Maximum operating power in 250 mW units
        pub raw_max_operating_power: u16 @ 0..=9,
    }
}

impl Battery {
    pub fn to_bytes(self, buf: &mut [u8]) {
        LittleEndian::write_u32(buf, self.0);
    }

    pub fn operating_power(&self) -> Power {
        Power::from_250mw_units(self.raw_operating_power())
    }

    pub fn max_operating_power(&self) -> Power {
        Power::from_250mw_units(self.raw_max_operating_power())
    }
}

bitfield!(
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Pps(pub u32): Debug, FromStorage, IntoStorage {
        /// Object position (0000b and 1110b…1111b are Reserved and Shall Not be used)
        pub object_position: u8 @ 28..=31,
        /// Capability mismatch
        pub capability_mismatch: bool @ 26,
        /// USB communications capable
        pub usb_communications_capable: bool @ 25,
        /// No USB Suspend
        pub no_usb_suspend: bool @ 24,
        /// Unchunked extended messages supported.
        /// WARNING: Do not set to true - the library always uses chunked mode
        /// for compatibility with more PHYs.
        pub unchunked_extended_messages_supported: bool @ 23,
        /// EPR mode capable
        pub epr_mode_capable: bool @ 22,
        /// Output voltage in 20 mV units
        pub raw_output_voltage: u16 @ 9..=20,
        /// Operating current in 50 mA units
        pub raw_operating_current: u16 @ 0..=6,
    }
);

impl Pps {
    pub fn to_bytes(self, buf: &mut [u8]) -> usize {
        LittleEndian::write_u32(buf, self.0);
        4
    }

    pub fn output_voltage(&self) -> ElectricPotential {
        ElectricPotential::from_20mv_units(self.raw_output_voltage())
    }

    pub fn operating_current(&self) -> ElectricCurrent {
        ElectricCurrent::from_50ma_units(self.raw_operating_current())
    }
}

bitfield!(
    #[derive(Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct Avs(pub u32): Debug, FromStorage, IntoStorage {
        /// Object position (0000b and 1110b…1111b are Reserved and Shall Not be used)
        pub object_position: u8 @ 28..=31,
        /// Capability mismatch
        pub capability_mismatch: bool @ 26,
        /// USB communications capable
        pub usb_communications_capable: bool @ 25,
        /// No USB Suspend
        pub no_usb_suspend: bool @ 24,
        /// Unchunked extended messages supported.
        /// WARNING: Do not set to true - the library always uses chunked mode
        /// for compatibility with more PHYs.
        pub unchunked_extended_messages_supported: bool @ 23,
        /// EPR mode capable
        pub epr_mode_capable: bool @ 22,
        /// Output voltage in 25 mV units (per USB PD 3.2 Table 6.26).
        /// The least two significant bits Shall be set to zero, making
        /// the effective voltage step size 100 mV.
        pub raw_output_voltage: u16 @ 9..=20,
        /// Operating current in 50 mA units
        pub raw_operating_current: u16 @ 0..=6,
    }
);

impl Avs {
    pub fn to_bytes(self, buf: &mut [u8]) -> usize {
        LittleEndian::write_u32(buf, self.0);
        4
    }

    pub fn output_voltage(&self) -> ElectricPotential {
        ElectricPotential::from_25mv_units(self.raw_output_voltage())
    }

    pub fn operating_current(&self) -> ElectricCurrent {
        ElectricCurrent::from_50ma_units(self.raw_operating_current())
    }
}

/// EPR Request containing RDO + copy of requested PDO for source verification.
///
/// Per USB PD 3.x Section 6.4.9, EPR_Request always has 2 data objects:
/// - The Request Data Object (format depends on PDO type being requested)
/// - Copy of the PDO being requested (for source verification)
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EprRequestDataObject {
    /// The raw Request Data Object (format depends on PDO type being requested).
    /// This could be a FixedVariableSupply RDO, Avs RDO, or other EPR RDO type.
    pub rdo: u32,
    /// Copy of the PDO being requested (for source verification)
    pub pdo: source_capabilities::PowerDataObject,
}

impl EprRequestDataObject {
    /// Get the object position from the RDO
    pub fn object_position(&self) -> u8 {
        RawDataObject(self.rdo).object_position()
    }
}

/// Power requests towards the source.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(unused)]
pub enum PowerSource {
    FixedVariableSupply(FixedVariableSupply),
    Battery(Battery),
    Pps(Pps),
    Avs(Avs),
    /// EPR Request: RDO + copy of requested PDO for source verification.
    EprRequest(EprRequestDataObject),
    Unknown(RawDataObject),
}

/// Errors that can occur during sink requests towards the source.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A requested (specific) voltage does not exist in the PDOs.
    VoltageMismatch,
}

/// Requestable voltage levels.
#[derive(Debug)]
pub enum VoltageRequest {
    /// The safe 5 V supply.
    Safe5V,
    /// The highest voltage that the source can supply.
    Highest,
    /// A specific voltage.
    Specific(ElectricPotential),
}

/// Requestable currents.
#[derive(Debug)]
pub enum CurrentRequest {
    /// The highest current that the source can supply.
    Highest,
    /// A specific current.
    Specific(ElectricCurrent),
}

/// A fixed supply PDO, alongside its index in the PDO table.
pub struct IndexedFixedSupply<'d>(pub &'d source_capabilities::FixedSupply, usize);

/// An augmented PDO, alongside its index in the PDO table.
pub struct IndexedAugmented<'d>(pub &'d source_capabilities::Augmented, usize);

impl PowerSource {
    pub fn object_position(&self) -> u8 {
        match self {
            PowerSource::FixedVariableSupply(p) => p.object_position(),
            PowerSource::Battery(p) => p.object_position(),
            PowerSource::Pps(p) => p.object_position(),
            PowerSource::Avs(p) => p.object_position(),
            PowerSource::EprRequest(epr) => epr.object_position(),
            PowerSource::Unknown(p) => p.object_position(),
        }
    }

    /// Determine the data message type to use for this request.
    pub fn message_type(&self) -> crate::protocol_layer::message::header::DataMessageType {
        match self {
            PowerSource::EprRequest { .. } => crate::protocol_layer::message::header::DataMessageType::EprRequest,
            _ => crate::protocol_layer::message::header::DataMessageType::Request,
        }
    }

    /// Number of data objects required to encode this request.
    pub fn num_objects(&self) -> u8 {
        match self {
            PowerSource::EprRequest { .. } => 2,
            _ => 1,
        }
    }

    /// Find the highest fixed voltage that can be found in the source capabilities.
    ///
    /// Reports the index of the found PDO, and the fixed supply instance, or `None` if there is no fixed supply PDO.
    pub fn find_highest_fixed_voltage(
        source_capabilities: &source_capabilities::SourceCapabilities,
    ) -> Option<IndexedFixedSupply<'_>> {
        let mut selected_pdo = None;

        for (index, cap) in source_capabilities.pdos().iter().enumerate() {
            if let source_capabilities::PowerDataObject::FixedSupply(fixed_supply) = cap {
                selected_pdo = match selected_pdo {
                    None => Some(IndexedFixedSupply(fixed_supply, index)),
                    Some(ref x) => {
                        if fixed_supply.voltage() > x.0.voltage() {
                            Some(IndexedFixedSupply(fixed_supply, index))
                        } else {
                            selected_pdo
                        }
                    }
                };
            }
        }

        selected_pdo
    }

    /// Find a specific fixed voltage within the source capabilities.
    ///
    /// Reports the index of the found PDO, and the fixed supply instance, or `None` if there is no match to the request.
    pub fn find_specific_fixed_voltage(
        source_capabilities: &source_capabilities::SourceCapabilities,
        voltage: ElectricPotential,
    ) -> Option<IndexedFixedSupply<'_>> {
        for (index, cap) in source_capabilities.pdos().iter().enumerate() {
            if let source_capabilities::PowerDataObject::FixedSupply(fixed_supply) = cap
                && (fixed_supply.voltage() == voltage)
            {
                return Some(IndexedFixedSupply(fixed_supply, index));
            }
        }

        None
    }

    /// Find a suitable Augmented PDO (PPS or AVS) by evaluating the provided voltage
    /// request against the source capabilities.
    ///
    /// This searches SPR PPS, SPR AVS, and EPR AVS PDOs for a matching voltage range.
    ///
    /// Reports the index of the found PDO, and the augmented supply instance, or `None` if there is no match to the request.
    pub fn find_augmented_pdo(
        source_capabilities: &source_capabilities::SourceCapabilities,
        voltage: ElectricPotential,
    ) -> Option<IndexedAugmented<'_>> {
        for (index, cap) in source_capabilities.pdos().iter().enumerate() {
            let source_capabilities::PowerDataObject::Augmented(augmented) = cap else {
                trace!("Skip non-augmented PDO {:?}", cap);
                continue;
            };

            match augmented {
                source_capabilities::Augmented::Spr(spr) => {
                    if spr.min_voltage() <= voltage && spr.max_voltage() >= voltage {
                        return Some(IndexedAugmented(augmented, index));
                    } else {
                        trace!("Skip PDO, voltage out of range. {:?}", augmented);
                    }
                }
                source_capabilities::Augmented::SprAvs(avs) => {
                    if avs.min_voltage() <= voltage && avs.max_voltage() >= voltage {
                        return Some(IndexedAugmented(augmented, index));
                    } else {
                        trace!("Skip PDO, voltage out of range. {:?}", augmented);
                    }
                }
                source_capabilities::Augmented::Epr(avs) => {
                    if avs.min_voltage() <= voltage && avs.max_voltage() >= voltage {
                        return Some(IndexedAugmented(augmented, index));
                    } else {
                        trace!("Skip PDO, voltage out of range. {:?}", augmented);
                    }
                }
                _ => trace!("Skip unknown augmented PDO. {:?}", augmented),
            };
        }

        trace!("Could not find suitable augmented PDO for voltage");
        None
    }

    /// Create a new, specific power source request for a fixed supply.
    ///
    /// # Arguments
    ///
    /// * `supply` - The combination of fixed supply PDO and its index in the PDO table.
    /// * `current_request` - The desired current level.
    pub fn new_fixed_specific(supply: IndexedFixedSupply, current_request: CurrentRequest) -> Result<Self, Error> {
        let IndexedFixedSupply(pdo, index) = supply;

        let (current, mismatch) = match current_request {
            CurrentRequest::Highest => (pdo.max_current(), false),
            CurrentRequest::Specific(x) => (x, x > pdo.max_current()),
        };

        let raw_current_unclamped = current.as_milliamps() / 10;
        let raw_current = raw_current_unclamped.min(0x3ff) as u16;

        if raw_current_unclamped > 0x3ff {
            error!("Clamping invalid current: {} mA", current.as_milliamps());
        }

        let object_position = index + 1;
        assert!(object_position > 0b0000 && object_position <= 0b1110);

        Ok(Self::FixedVariableSupply(
            FixedVariableSupply(0)
                .with_raw_operating_current(raw_current)
                .with_raw_max_operating_current(raw_current)
                .with_object_position(object_position as u8)
                .with_capability_mismatch(mismatch)
                .with_no_usb_suspend(true)
                .with_usb_communications_capable(true), // FIXME: Make adjustable?
        ))
    }

    /// Create a new power source request for a fixed supply.
    ///
    /// Finds a suitable PDO by evaluating the provided current and voltage requests against the source capabilities.
    pub fn new_fixed(
        current_request: CurrentRequest,
        voltage_request: VoltageRequest,
        source_capabilities: &source_capabilities::SourceCapabilities,
    ) -> Result<Self, Error> {
        let selected = match voltage_request {
            VoltageRequest::Safe5V => source_capabilities.vsafe_5v().map(|supply| IndexedFixedSupply(supply, 0)),
            VoltageRequest::Highest => Self::find_highest_fixed_voltage(source_capabilities),
            VoltageRequest::Specific(x) => Self::find_specific_fixed_voltage(source_capabilities, x),
        };

        if selected.is_none() {
            return Err(Error::VoltageMismatch);
        }

        Self::new_fixed_specific(selected.unwrap(), current_request)
    }

    /// Create a new power source request for a programmable power supply (PPS).
    ///
    /// Finds a suitable PDO by evaluating the provided current and voltage requests against the source capabilities.
    /// If no PDO is found that matches the request, an error is returned.
    pub fn new_pps(
        current_request: CurrentRequest,
        voltage: ElectricPotential,
        source_capabilities: &source_capabilities::SourceCapabilities,
    ) -> Result<Self, Error> {
        let selected = Self::find_augmented_pdo(source_capabilities, voltage);

        if selected.is_none() {
            return Err(Error::VoltageMismatch);
        }

        let IndexedAugmented(pdo, index) = selected.unwrap();
        let max_current = match pdo {
            source_capabilities::Augmented::Spr(spr) => spr.max_current(),
            _ => return Err(Error::VoltageMismatch),
        };

        let (current, mismatch) = match current_request {
            CurrentRequest::Highest => (max_current, false),
            CurrentRequest::Specific(x) => (x, x > max_current),
        };

        let raw_current_unclamped = current.as_milliamps() / 50;
        let raw_current = raw_current_unclamped.min(0x7f) as u16;

        if raw_current_unclamped > 0x7f {
            error!("Clamping invalid PPS current: {} mA", current.as_milliamps());
        }

        let raw_voltage = (voltage.as_millivolts() / 20).min(0xfff) as u16;

        let object_position = index + 1;
        assert!(object_position > 0b0000 && object_position <= 0b1110);

        Ok(Self::Pps(
            Pps(0)
                .with_raw_output_voltage(raw_voltage)
                .with_raw_operating_current(raw_current)
                .with_object_position(object_position as u8)
                .with_capability_mismatch(mismatch)
                .with_no_usb_suspend(true)
                .with_usb_communications_capable(true),
        ))
    }

    /// Create a new EPR AVS request.
    ///
    /// Per USB PD 3.x Section 6.4.9, this creates an EPR_Request with an AVS RDO
    /// and a copy of the requested PDO.
    pub fn new_epr_avs(
        current_request: CurrentRequest,
        voltage: ElectricPotential,
        source_capabilities: &source_capabilities::SourceCapabilities,
    ) -> Result<Self, Error> {
        let selected = Self::find_augmented_pdo(source_capabilities, voltage);

        if selected.is_none() {
            return Err(Error::VoltageMismatch);
        }

        let IndexedAugmented(pdo, index) = selected.unwrap();
        let max_current = match pdo {
            source_capabilities::Augmented::Epr(avs) => {
                avs.pd_power().checked_current_at(voltage).ok_or(Error::VoltageMismatch)?
            }
            _ => return Err(Error::VoltageMismatch),
        };

        let (current, mismatch) = match current_request {
            CurrentRequest::Highest => (max_current, false),
            CurrentRequest::Specific(x) => (x, x > max_current),
        };

        let raw_current_unclamped = current.as_milliamps() / 50;
        let raw_current = raw_current_unclamped.min(0x7f) as u16;

        if raw_current_unclamped > 0x7f {
            error!("Clamping invalid AVS current: {} mA", current.as_milliamps());
        }

        // AVS voltage is in 25 mV units with LSB 2 bits = 0 (effective 100 mV steps)
        // Per USB PD 3.2 Table 6.26: "Output voltage in 25 mV units,
        // the least two significant bits Shall be set to zero"
        let raw_voltage = ((voltage.as_millivolts() / 25).min(0xfff) as u16) & !0x3;

        let object_position = index + 1;
        assert!(object_position > 0b0000 && object_position <= 0b1110);

        // Build AVS RDO (Table 6.26)
        let rdo = Avs(0)
            .with_raw_output_voltage(raw_voltage)
            .with_raw_operating_current(raw_current)
            .with_object_position(object_position as u8)
            .with_capability_mismatch(mismatch)
            .with_no_usb_suspend(true)
            .with_usb_communications_capable(true)
            .with_epr_mode_capable(true)
            .0;

        // Copy of the PDO being requested
        let pdo_copy = source_capabilities::PowerDataObject::Augmented(*pdo);

        Ok(Self::EprRequest(EprRequestDataObject { rdo, pdo: pdo_copy }))
    }
}

#[cfg(test)]
mod tests {
    use heapless::Vec;

    use super::{Avs, CurrentRequest, PowerSource, Pps, VoltageRequest};
    use crate::protocol_layer::message::data::source_capabilities::{
        MAX_EPR_SOURCE_PDOS, PowerDataObject, SourceCapabilities, parse_raw_pdo,
    };
    use crate::units::{ElectricCurrent, ElectricPotential};

    #[test]
    fn captured_adjustable_rdos_decode_to_the_same_quantities() {
        let pps = Pps(0x6148_3464);
        assert_eq!(pps.output_voltage().as_millivolts(), 21_000);
        assert_eq!(pps.operating_current().as_milliamps(), 5_000);

        let avs = Avs(0xb14f_0064);
        assert_eq!(avs.output_voltage().as_millivolts(), 48_000);
        assert_eq!(avs.operating_current().as_milliamps(), 5_000);
    }

    #[test]
    fn pps_request_encoding_matches_the_previous_wire_value() {
        let mut pdos = Vec::new();
        pdos.push(parse_raw_pdo(0x0a91_912c)).unwrap();
        pdos.push(parse_raw_pdo(0xc9a4_3264)).unwrap();
        let caps = SourceCapabilities::new_with_pdos(pdos);

        let PowerSource::Pps(request) =
            PowerSource::new_pps(CurrentRequest::Highest, ElectricPotential::from_millivolts(19_400), &caps).unwrap()
        else {
            panic!("PPS APDO must produce a PPS request")
        };

        assert_eq!(request.0, 0x2307_9464);
    }

    #[test]
    fn epr_power_to_current_floor_matches_the_previous_wire_value() {
        let mut pdos: Vec<PowerDataObject, 16> = Vec::new();
        for _ in 0..(MAX_EPR_SOURCE_PDOS - 1) {
            pdos.push(parse_raw_pdo(0)).unwrap();
        }
        // Captured 15-48 V, 140 W EPR AVS PDO.
        pdos.push(parse_raw_pdo(0xd7c0_968c)).unwrap();
        let caps = SourceCapabilities::new_with_pdos(pdos);

        let PowerSource::EprRequest(request) =
            PowerSource::new_epr_avs(CurrentRequest::Highest, ElectricPotential::from_volts(48), &caps).unwrap()
        else {
            panic!("EPR AVS APDO must produce an EPR request")
        };

        // 140 W / 48 V floors to 2916 mA; the RDO then floors that to
        // 58 current units of 50 mA, exactly as the former uom path did.
        assert_eq!(request.rdo, 0xb34f_003a);
        assert_eq!(request.pdo.to_raw(), 0xd7c0_968c);
    }

    #[test]
    fn fixed_request_current_saturates_at_the_wire_field_maximum() {
        let mut pdos = Vec::new();
        pdos.push(parse_raw_pdo(0x0a91_912c)).unwrap();
        let caps = SourceCapabilities::new_with_pdos(pdos);

        let PowerSource::FixedVariableSupply(request) = PowerSource::new_fixed(
            CurrentRequest::Specific(ElectricCurrent::from_milliamps(20_000)),
            VoltageRequest::Safe5V,
            &caps,
        )
        .unwrap() else {
            panic!("fixed PDO must produce a fixed request")
        };

        assert_eq!(request.raw_operating_current(), 0x3ff);
        assert_eq!(request.raw_max_operating_current(), 0x3ff);
    }

    #[test]
    fn pps_request_current_saturates_at_the_wire_field_maximum() {
        let mut pdos = Vec::new();
        pdos.push(parse_raw_pdo(0x0a91_912c)).unwrap();
        pdos.push(parse_raw_pdo(0xc9a4_3264)).unwrap();
        let caps = SourceCapabilities::new_with_pdos(pdos);

        let PowerSource::Pps(request) = PowerSource::new_pps(
            CurrentRequest::Specific(ElectricCurrent::from_milliamps(6_400)),
            ElectricPotential::from_millivolts(19_400),
            &caps,
        )
        .unwrap() else {
            panic!("PPS APDO must produce a PPS request")
        };

        assert_eq!(request.raw_operating_current(), 0x7f);
    }

    #[test]
    fn avs_request_current_saturates_at_the_wire_field_maximum() {
        let mut pdos: Vec<PowerDataObject, 16> = Vec::new();
        for _ in 0..(MAX_EPR_SOURCE_PDOS - 1) {
            pdos.push(parse_raw_pdo(0)).unwrap();
        }
        pdos.push(parse_raw_pdo(0xd7c0_968c)).unwrap();
        let caps = SourceCapabilities::new_with_pdos(pdos);

        let PowerSource::EprRequest(request) = PowerSource::new_epr_avs(
            CurrentRequest::Specific(ElectricCurrent::from_milliamps(6_400)),
            ElectricPotential::from_volts(48),
            &caps,
        )
        .unwrap() else {
            panic!("EPR AVS APDO must produce an EPR request")
        };

        assert_eq!(Avs(request.rdo).raw_operating_current(), 0x7f);
    }
}
