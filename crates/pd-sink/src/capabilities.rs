use crate::units::{Milliamps, Millivolts, Milliwatts};

pub const MAX_SOURCE_PDOS: usize = 11;
pub const EPR_AVS_STANDARD_MIN_VOLTAGE: Millivolts = Millivolts(15_000);
const EPR_AVS_COMPATIBLE_MIN_VOLTAGE: Millivolts = Millivolts(5_000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilitiesKind {
    Spr,
    Epr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityListError {
    Empty,
    TooMany { supplied: usize, maximum: usize },
    EprListTooShort { supplied: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdoError {
    InvalidFirstPdo,
    InvalidPosition,
    InvalidVoltage,
    InvalidVoltageRange,
    InvalidCurrent,
    InvalidPower,
    UnexpectedZero,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdoValidity {
    Valid,
    /// Outside the strict USB-PD power rules, but bounded such that this sink
    /// can issue a safe request within the advertised range and its own limits.
    Compatible,
    ZeroPadding,
    Malformed(PdoError),
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupplyKind {
    Fixed,
    Pps,
    SprAvs,
    EprAvs,
    ZeroPadding,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedSupply {
    pub voltage: Millivolts,
    pub max_current: Milliamps,
    pub epr_mode_capable: bool,
    pub unconstrained_power: bool,
    pub usb_communications_capable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PpsSupply {
    pub min_voltage: Millivolts,
    pub max_voltage: Millivolts,
    pub max_current: Milliamps,
    pub power_limited: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SprAvsSupply {
    pub min_voltage: Millivolts,
    pub max_voltage: Millivolts,
    pub max_current_15v: Milliamps,
    pub max_current_20v: Milliamps,
    pub peak_current: u8,
}

impl SprAvsSupply {
    pub fn max_current_at(self, voltage: Millivolts) -> Option<Milliamps> {
        if voltage < self.min_voltage || voltage > self.max_voltage {
            None
        } else if voltage <= Millivolts(15_000) {
            Some(self.max_current_15v)
        } else {
            Some(self.max_current_20v)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EprAvsSupply {
    pub min_voltage: Millivolts,
    pub max_voltage: Millivolts,
    pub pdp: Milliwatts,
    pub peak_current: u8,
}

impl EprAvsSupply {
    /// The portion of an advertised EPR AVS range that follows the USB-PD
    /// definition. Some real sources extend the lower bound below 15 V.
    pub const fn standard_min_voltage(self) -> Millivolts {
        if self.min_voltage.0 < EPR_AVS_STANDARD_MIN_VOLTAGE.0 {
            EPR_AVS_STANDARD_MIN_VOLTAGE
        } else {
            self.min_voltage
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceSupply {
    Fixed(FixedSupply),
    Pps(PpsSupply),
    SprAvs(SprAvsSupply),
    EprAvs(EprAvsSupply),
    ZeroPadding,
    Unsupported { pdo_type: u8, apdo_type: Option<u8> },
}

impl SourceSupply {
    pub const fn kind(self) -> SupplyKind {
        match self {
            Self::Fixed(_) => SupplyKind::Fixed,
            Self::Pps(_) => SupplyKind::Pps,
            Self::SprAvs(_) => SupplyKind::SprAvs,
            Self::EprAvs(_) => SupplyKind::EprAvs,
            Self::ZeroPadding => SupplyKind::ZeroPadding,
            Self::Unsupported { .. } => SupplyKind::Unsupported,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdvertisedPdo {
    pub position: u8,
    pub raw: u32,
    pub supply: SourceSupply,
    pub validity: PdoValidity,
}

impl AdvertisedPdo {
    pub const fn kind(self) -> SupplyKind {
        self.supply.kind()
    }

    pub const fn is_requestable(self) -> bool {
        matches!(self.validity, PdoValidity::Valid | PdoValidity::Compatible)
    }

    pub const fn requires_epr_mode(self) -> bool {
        self.position >= 8 || matches!(self.supply, SourceSupply::EprAvs(_))
    }

    pub fn voltage_range(self) -> Option<(Millivolts, Millivolts)> {
        match self.supply {
            SourceSupply::Fixed(fixed) => Some((fixed.voltage, fixed.voltage)),
            SourceSupply::Pps(pps) => Some((pps.min_voltage, pps.max_voltage)),
            SourceSupply::SprAvs(avs) => Some((avs.min_voltage, avs.max_voltage)),
            SourceSupply::EprAvs(avs) => Some((avs.min_voltage, avs.max_voltage)),
            SourceSupply::ZeroPadding | SourceSupply::Unsupported { .. } => None,
        }
    }

    /// Range used by normal automatic and supply-family selection.
    ///
    /// Direct PDO selection can still explicitly use the complete advertised
    /// range of a bounded compatible offer.
    pub fn standard_voltage_range(self) -> Option<(Millivolts, Millivolts)> {
        match self.supply {
            SourceSupply::EprAvs(avs) => Some((avs.standard_min_voltage(), avs.max_voltage)),
            _ => self.voltage_range(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceCapabilities {
    kind: CapabilitiesKind,
    raw: [u32; MAX_SOURCE_PDOS],
    len: u8,
}

impl SourceCapabilities {
    pub fn new(kind: CapabilitiesKind, pdos: &[u32]) -> Result<Self, CapabilityListError> {
        if pdos.is_empty() {
            return Err(CapabilityListError::Empty);
        }
        if pdos.len() > MAX_SOURCE_PDOS {
            return Err(CapabilityListError::TooMany { supplied: pdos.len(), maximum: MAX_SOURCE_PDOS });
        }
        if matches!(kind, CapabilitiesKind::Spr) && pdos.len() > 7 {
            return Err(CapabilityListError::TooMany { supplied: pdos.len(), maximum: 7 });
        }
        if matches!(kind, CapabilitiesKind::Epr) && pdos.len() < 8 {
            return Err(CapabilityListError::EprListTooShort { supplied: pdos.len() });
        }

        let mut raw = [0; MAX_SOURCE_PDOS];
        raw[..pdos.len()].copy_from_slice(pdos);
        Ok(Self { kind, raw, len: pdos.len() as u8 })
    }

    pub const fn kind(&self) -> CapabilitiesKind {
        self.kind
    }

    pub const fn len(&self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn raw_pdos(&self) -> &[u32] {
        &self.raw[..self.len()]
    }

    pub fn pdo(&self, position: u8) -> Option<AdvertisedPdo> {
        if position == 0 || usize::from(position) > self.len() {
            return None;
        }

        Some(parse_pdo(self.kind, position, self.raw[usize::from(position - 1)]))
    }

    pub fn iter(&self) -> CapabilityIter<'_> {
        CapabilityIter { capabilities: self, next_position: 1 }
    }

    pub fn epr_mode_capable(&self) -> bool {
        matches!(
            self.pdo(1),
            Some(AdvertisedPdo {
                supply: SourceSupply::Fixed(FixedSupply { epr_mode_capable: true, .. }),
                validity: PdoValidity::Valid,
                ..
            })
        )
    }
}

pub struct CapabilityIter<'a> {
    capabilities: &'a SourceCapabilities,
    next_position: u8,
}

impl Iterator for CapabilityIter<'_> {
    type Item = AdvertisedPdo;

    fn next(&mut self) -> Option<Self::Item> {
        let pdo = self.capabilities.pdo(self.next_position)?;
        self.next_position += 1;
        Some(pdo)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.capabilities.len().saturating_sub(usize::from(self.next_position - 1));
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for CapabilityIter<'_> {}

fn parse_pdo(kind: CapabilitiesKind, position: u8, raw: u32) -> AdvertisedPdo {
    if raw == 0 {
        let valid_padding = matches!(kind, CapabilitiesKind::Epr) && (2..=7).contains(&position);
        return AdvertisedPdo {
            position,
            raw,
            supply: SourceSupply::ZeroPadding,
            validity: if valid_padding {
                PdoValidity::ZeroPadding
            } else {
                PdoValidity::Malformed(PdoError::UnexpectedZero)
            },
        };
    }

    let pdo_type = ((raw >> 30) & 0x3) as u8;
    match pdo_type {
        0b00 => parse_fixed(kind, position, raw),
        0b01 | 0b10 => parse_unsupported_legacy(position, raw, pdo_type),
        0b11 => parse_augmented(kind, position, raw),
        _ => unreachable!(),
    }
}

fn advertised(position: u8, raw: u32, supply: SourceSupply, error: Option<PdoError>) -> AdvertisedPdo {
    AdvertisedPdo { position, raw, supply, validity: error.map_or(PdoValidity::Valid, PdoValidity::Malformed) }
}

fn parse_fixed(kind: CapabilitiesKind, position: u8, raw: u32) -> AdvertisedPdo {
    let supply = FixedSupply {
        voltage: Millivolts(((raw >> 10) & 0x3ff) * 50),
        max_current: Milliamps((raw & 0x3ff) * 10),
        epr_mode_capable: raw & (1 << 23) != 0,
        unconstrained_power: raw & (1 << 27) != 0,
        usb_communications_capable: raw & (1 << 26) != 0,
    };

    let error = if position == 1 && supply.voltage != Millivolts(5_000) {
        Some(PdoError::InvalidFirstPdo)
    } else if supply.max_current == Milliamps(0) || supply.max_current > Milliamps(5_000) {
        Some(PdoError::InvalidCurrent)
    } else if (position <= 7 && supply.voltage > Millivolts(20_000))
        || (position >= 8
            && (!matches!(kind, CapabilitiesKind::Epr) || !matches!(supply.voltage.0, 28_000 | 36_000 | 48_000)))
    {
        Some(PdoError::InvalidPosition)
    } else if supply.voltage == Millivolts(0) {
        Some(PdoError::InvalidVoltage)
    } else {
        None
    };

    advertised(position, raw, SourceSupply::Fixed(supply), error)
}

fn parse_unsupported_legacy(position: u8, raw: u32, pdo_type: u8) -> AdvertisedPdo {
    let validity = if position == 1 {
        PdoValidity::Malformed(PdoError::InvalidFirstPdo)
    } else if position > 7 {
        PdoValidity::Malformed(PdoError::InvalidPosition)
    } else {
        PdoValidity::Unsupported
    };
    AdvertisedPdo { position, raw, supply: SourceSupply::Unsupported { pdo_type, apdo_type: None }, validity }
}

fn parse_augmented(kind: CapabilitiesKind, position: u8, raw: u32) -> AdvertisedPdo {
    let apdo_type = ((raw >> 28) & 0x3) as u8;
    match apdo_type {
        0b00 => {
            let supply = PpsSupply {
                max_voltage: Millivolts(((raw >> 17) & 0xff) * 100),
                min_voltage: Millivolts(((raw >> 8) & 0xff) * 100),
                max_current: Milliamps((raw & 0x7f) * 50),
                power_limited: raw & (1 << 27) != 0,
            };
            let validity = if position == 1 {
                PdoValidity::Malformed(PdoError::InvalidFirstPdo)
            } else if position > 7 {
                PdoValidity::Malformed(PdoError::InvalidPosition)
            } else if supply.min_voltage < Millivolts(3_300)
                || supply.max_voltage > Millivolts(21_000)
                || supply.min_voltage > supply.max_voltage
            {
                PdoValidity::Malformed(PdoError::InvalidVoltageRange)
            } else if supply.max_current == Milliamps(0) {
                PdoValidity::Malformed(PdoError::InvalidCurrent)
            } else if !matches!(supply.min_voltage.0, 3_300 | 5_000)
                || !matches!(supply.max_voltage.0, 11_000 | 16_000 | 21_000)
                || supply.max_current > Milliamps(5_000)
            {
                // Real chargers use proprietary but wire-compatible PPS
                // ranges (for example 3.6-21 V or 4.5-11 V) and sometimes
                // advertise more than the USB-C 5 A ceiling. Keep the offer,
                // but let the request planner cap every request to 5 A and
                // the configured board/cable limits.
                PdoValidity::Compatible
            } else {
                PdoValidity::Valid
            };
            AdvertisedPdo { position, raw, supply: SourceSupply::Pps(supply), validity }
        }
        0b01 => {
            let supply = EprAvsSupply {
                max_voltage: Millivolts(((raw >> 17) & 0x1ff) * 100),
                min_voltage: Millivolts(((raw >> 8) & 0xff) * 100),
                pdp: Milliwatts((raw & 0xff) * 1_000),
                peak_current: ((raw >> 26) & 0x3) as u8,
            };
            let validity = if position < 8 || !matches!(kind, CapabilitiesKind::Epr) {
                PdoValidity::Malformed(PdoError::InvalidPosition)
            } else if !matches!(supply.max_voltage.0, 28_000 | 36_000 | 48_000)
                || supply.min_voltage < EPR_AVS_COMPATIBLE_MIN_VOLTAGE
                || supply.min_voltage > supply.max_voltage
            {
                PdoValidity::Malformed(PdoError::InvalidVoltageRange)
            } else if supply.pdp == Milliwatts(0) || supply.pdp > Milliwatts(240_000) {
                PdoValidity::Malformed(PdoError::InvalidPower)
            } else if supply.min_voltage != EPR_AVS_STANDARD_MIN_VOLTAGE {
                // Early high-power sources have shipped a wire-compatible
                // 5 V lower bound. Preserve that advertised range for an
                // explicit opt-in, while normal planning starts at 15 V.
                PdoValidity::Compatible
            } else {
                PdoValidity::Valid
            };
            AdvertisedPdo { position, raw, supply: SourceSupply::EprAvs(supply), validity }
        }
        0b10 => {
            let max_current_15v = Milliamps(((raw >> 10) & 0x3ff) * 10);
            let max_current_20v = Milliamps((raw & 0x3ff) * 10);
            let supply = SprAvsSupply {
                min_voltage: Millivolts(9_000),
                max_voltage: if max_current_20v == Milliamps(0) { Millivolts(15_000) } else { Millivolts(20_000) },
                max_current_15v,
                max_current_20v,
                peak_current: ((raw >> 26) & 0x3) as u8,
            };
            let error = if position == 1 {
                Some(PdoError::InvalidFirstPdo)
            } else if position > 7 {
                Some(PdoError::InvalidPosition)
            } else if supply.max_current_15v == Milliamps(0)
                || supply.max_current_15v > Milliamps(5_000)
                || supply.max_current_20v > Milliamps(5_000)
            {
                Some(PdoError::InvalidCurrent)
            } else {
                None
            };
            advertised(position, raw, SourceSupply::SprAvs(supply), error)
        }
        _ => AdvertisedPdo {
            position,
            raw,
            supply: SourceSupply::Unsupported { pdo_type: 0b11, apdo_type: Some(apdo_type) },
            validity: PdoValidity::Unsupported,
        },
    }
}
