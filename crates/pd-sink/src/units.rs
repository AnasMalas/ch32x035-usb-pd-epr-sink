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

fn fractional_current(remainder: u32, voltage: u32) -> u32 {
    debug_assert!(remainder < voltage);

    if let Some(scaled) = remainder.checked_mul(1_000) {
        return scaled / voltage;
    }

    // The result is below 1,000 because `remainder < voltage`. Divide the
    // denominator into thousands so neither side of the comparison can
    // overflow. The initial estimate can exceed the exact floor by at most
    // one when this branch is reachable.
    let voltage_thousands = voltage / 1_000;
    let voltage_remainder = voltage % 1_000;
    let candidate = (remainder / voltage_thousands).min(999);
    let threshold = candidate * voltage_thousands + (candidate * voltage_remainder).div_ceil(1_000);
    candidate - u32::from(threshold > remainder)
}

pub(crate) fn current_for_power(power: Milliwatts, voltage: Millivolts) -> Milliamps {
    if voltage.0 == 0 {
        return Milliamps(0);
    }

    let whole = power.0 / voltage.0;
    if whole > u32::MAX / 1_000 {
        return Milliamps(u32::MAX);
    }

    let fraction = fractional_current(power.0 % voltage.0, voltage.0);
    Milliamps(whole.saturating_mul(1_000).saturating_add(fraction))
}

#[cfg(test)]
mod tests {
    use super::{current_for_power, Milliamps, Millivolts, Milliwatts};

    fn reference(power: u32, voltage: u32) -> u32 {
        if voltage == 0 {
            return 0;
        }
        ((u64::from(power) * 1_000) / u64::from(voltage)).min(u64::from(u32::MAX)) as u32
    }

    #[test]
    fn power_to_current_matches_wide_reference_at_boundaries() {
        let values = [0, 1, 999, 1_000, 4_294_966, 4_294_967, 4_294_968, u32::MAX / 2, u32::MAX - 1, u32::MAX];

        for power in values {
            for voltage in values {
                assert_eq!(
                    current_for_power(Milliwatts(power), Millivolts(voltage)),
                    Milliamps(reference(power, voltage)),
                    "power={power}, voltage={voltage}"
                );
            }
        }
    }

    #[test]
    fn power_to_current_matches_wide_reference_for_varied_inputs() {
        let mut power = 0x243f_6a88_u32;
        let mut voltage = 0x85a3_08d3_u32;
        for _ in 0..100_000 {
            power = power.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            voltage = voltage.wrapping_mul(22_695_477).wrapping_add(1);
            assert_eq!(
                current_for_power(Milliwatts(power), Millivolts(voltage)),
                Milliamps(reference(power, voltage)),
                "power={power}, voltage={voltage}"
            );
        }
    }
}
