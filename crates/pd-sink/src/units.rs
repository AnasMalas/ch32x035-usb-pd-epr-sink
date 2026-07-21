use core::fmt;

macro_rules! unit {
    ($name:ident, $suffix:literal) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $name(pub u32);

        impl $name {
            pub const fn new(value: u32) -> Self {
                Self(value)
            }

            pub const fn get(self) -> u32 {
                self.0
            }
        }

        impl From<u32> for $name {
            fn from(value: u32) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{} {}", self.0, $suffix)
            }
        }
    };
}

unit!(Millivolts, "mV");
unit!(Milliamps, "mA");
unit!(Milliwatts, "mW");

pub(crate) const fn floor_to(value: u32, step: u32) -> u32 {
    value / step * step
}

pub(crate) fn current_for_power(power: Milliwatts, voltage: Millivolts) -> Milliamps {
    if voltage.0 == 0 {
        return Milliamps(0);
    }

    Milliamps(((u64::from(power.0) * 1_000) / u64::from(voltage.0)).min(u64::from(u32::MAX)) as u32)
}
