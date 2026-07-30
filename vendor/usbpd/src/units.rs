//! Small integer electrical units used by the USB-PD wire formats.
//!
//! USB-PD encodes voltage, current, and power as unsigned integers with
//! fixed, decimal step sizes. Keeping each quantity in its smallest common
//! unit makes those conversions explicit and avoids pulling a general
//! dimensional-analysis and rational-arithmetic implementation into the
//! firmware.

/// An electrical potential stored in millivolts.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ElectricPotential(u32);

impl ElectricPotential {
    /// Construct a potential from millivolts.
    pub const fn from_millivolts(millivolts: u32) -> Self {
        Self(millivolts)
    }

    /// Construct a potential from whole volts.
    pub const fn from_volts(volts: u16) -> Self {
        Self((volts as u32) * 1_000)
    }

    /// Construct a potential from a 20 mV USB-PD field.
    pub const fn from_20mv_units(raw: u16) -> Self {
        Self((raw as u32) * 20)
    }

    /// Construct a potential from a 25 mV USB-PD field.
    pub const fn from_25mv_units(raw: u16) -> Self {
        Self((raw as u32) * 25)
    }

    /// Construct a potential from a 50 mV USB-PD field.
    pub const fn from_50mv_units(raw: u16) -> Self {
        Self((raw as u32) * 50)
    }

    /// Construct a potential from a 100 mV USB-PD field.
    pub const fn from_100mv_units(raw: u16) -> Self {
        Self((raw as u32) * 100)
    }

    /// Return the stored millivolt value.
    pub const fn as_millivolts(self) -> u32 {
        self.0
    }
}

/// An electrical current stored in milliamps.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ElectricCurrent(u32);

impl ElectricCurrent {
    /// Construct a current from milliamps.
    pub const fn from_milliamps(milliamps: u32) -> Self {
        Self(milliamps)
    }

    /// Construct a current from a 10 mA USB-PD field.
    pub const fn from_10ma_units(raw: u16) -> Self {
        Self((raw as u32) * 10)
    }

    /// Construct a current from a 50 mA USB-PD field.
    pub const fn from_50ma_units(raw: u16) -> Self {
        Self((raw as u32) * 50)
    }

    /// Return the stored milliamp value.
    pub const fn as_milliamps(self) -> u32 {
        self.0
    }
}

/// Electrical power stored in milliwatts.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Power(u32);

impl Power {
    /// Construct a power value from milliwatts.
    pub const fn from_milliwatts(milliwatts: u32) -> Self {
        Self(milliwatts)
    }

    /// Construct a power value from whole watts.
    pub const fn from_watts(watts: u8) -> Self {
        Self((watts as u32) * 1_000)
    }

    /// Construct a power value from a 250 mW USB-PD field.
    pub const fn from_250mw_units(raw: u16) -> Self {
        Self((raw as u32) * 250)
    }

    /// Return the stored milliwatt value.
    pub const fn as_milliwatts(self) -> u32 {
        self.0
    }

    /// Return whole watts, rounding down like the previous unit conversion.
    pub const fn as_watts_floor(self) -> Option<u8> {
        let watts = self.0 / 1_000;
        if watts > u8::MAX as u32 { None } else { Some(watts as u8) }
    }

    /// Calculate the current at a given potential, returning `None` for zero
    /// volts or when the intermediate milliwatt-to-milliamp conversion would
    /// overflow.
    pub const fn checked_current_at(self, potential: ElectricPotential) -> Option<ElectricCurrent> {
        if potential.0 == 0 {
            return None;
        }

        match self.0.checked_mul(1_000) {
            Some(milliwatt_milliamps) => Some(ElectricCurrent(milliwatt_milliamps / potential.0)),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ElectricCurrent, ElectricPotential, Power};

    #[test]
    fn wire_steps_convert_without_rounding() {
        assert_eq!(ElectricPotential::from_20mv_units(970).as_millivolts(), 19_400);
        assert_eq!(ElectricPotential::from_25mv_units(1_920).as_millivolts(), 48_000);
        assert_eq!(ElectricPotential::from_50mv_units(560).as_millivolts(), 28_000);
        assert_eq!(ElectricPotential::from_100mv_units(210).as_millivolts(), 21_000);
        assert_eq!(ElectricCurrent::from_10ma_units(500).as_milliamps(), 5_000);
        assert_eq!(ElectricCurrent::from_50ma_units(100).as_milliamps(), 5_000);
        assert_eq!(Power::from_250mw_units(560).as_milliwatts(), 140_000);
    }

    #[test]
    fn power_division_uses_the_previous_integer_floor() {
        assert_eq!(
            Power::from_watts(140).checked_current_at(ElectricPotential::from_volts(48)),
            Some(ElectricCurrent::from_milliamps(2_916))
        );
        assert_eq!(Power::from_watts(140).checked_current_at(ElectricPotential::default()), None);
    }

    #[test]
    fn whole_watt_conversion_preserves_the_previous_floor() {
        assert_eq!(Power::from_watts(240).as_watts_floor(), Some(240));
        assert_eq!(Power::from_milliwatts(140_500).as_watts_floor(), Some(140));
        assert_eq!(Power::from_milliwatts(256_000).as_watts_floor(), None);
    }
}
