//! # Library for USB PD
//!
//! Modeled after the Universal Serial Bus Power Delivery Specification: USB PD R3.2 v1.1 (2024/10).
//!
//! The library implements:
//! - A policy engine for each supported mode,
//! - the protocol layer, and
//! - the `DevicePolicyManager` trait, which allows a device user application to talk to the policy engine, and control it.
//!
//! ## Currently supported modes
//!
//! - SPR Sink with helpers for requesting
//! - A fixed supply
//! - A Programmable Power Supply (PPS)
//!

#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]

// This mod MUST go first, so that the others see its macros.
pub(crate) mod fmt;

#[cfg(feature = "numeric-trace")]
macro_rules! numeric_trace {
    ($event:expr) => {
        $crate::numeric_trace::emit($event)
    };
}

#[cfg(not(feature = "numeric-trace"))]
macro_rules! numeric_trace {
    ($event:expr) => {};
}

pub(crate) mod counters;
#[cfg(feature = "numeric-trace")]
pub mod numeric_trace;
pub mod protocol_layer;
pub mod sink;
#[cfg(any(feature = "source", test))]
pub mod source;
pub mod timers;
pub mod units;

#[cfg(test)]
#[allow(missing_docs)] // FIXME: Docs for the dummy?
pub mod dummy;

use core::fmt::Debug;

/// The power role of the port.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum PowerRole {
    /// The port is a source.
    Source,
    /// The port is a sink.
    Sink,
}

impl From<bool> for PowerRole {
    fn from(value: bool) -> Self {
        match value {
            false => Self::Sink,
            true => Self::Source,
        }
    }
}

impl From<PowerRole> for bool {
    fn from(role: PowerRole) -> bool {
        match role {
            PowerRole::Sink => false,
            PowerRole::Source => true,
        }
    }
}

/// The data role of the port.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum DataRole {
    /// The port is an upstream-facing port.
    Ufp,
    /// The port is a downstream-facing port.
    Dfp,
}

impl From<bool> for DataRole {
    fn from(value: bool) -> Self {
        match value {
            false => Self::Ufp,
            true => Self::Dfp,
        }
    }
}

impl From<DataRole> for bool {
    fn from(role: DataRole) -> bool {
        match role {
            DataRole::Ufp => false,
            DataRole::Dfp => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::units::{ElectricCurrent, ElectricPotential};

    #[test]
    fn test_units() {
        let current = ElectricCurrent::from_milliamps(123);
        let potential = ElectricPotential::from_millivolts(4560);

        assert_eq!(current.as_milliamps(), 123);
        assert_eq!(potential.as_millivolts(), 4560);
        assert_eq!(potential.as_millivolts() / 20, 228);
    }
}
