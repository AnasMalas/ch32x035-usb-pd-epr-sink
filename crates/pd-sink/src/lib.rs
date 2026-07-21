#![no_std]
#![forbid(unsafe_code)]

//! Product-owned, hardware-independent USB Power Delivery sink logic.
//!
//! This crate deliberately contains no executor, allocator, or CH32-specific
//! code. It is used by the firmware and can also be tested on a desktop host.

pub mod capabilities;
pub mod command;
pub mod contract;
pub mod controller;
pub mod request;
pub mod safety;
pub mod units;

pub use capabilities::{
    AdvertisedPdo, CapabilitiesKind, PdoError, PdoValidity, SourceCapabilities, SourceSupply, SupplyKind,
    EPR_AVS_STANDARD_MIN_VOLTAGE,
};
pub use command::{parse_command, Command, CommandError};
pub use contract::{ContractError, ContractState, ContractTracker};
pub use controller::{ControllerAction, ControllerConfig, ControllerError, EprState, SinkController, UserRequest};
pub use request::{
    CurrentConfidence, Demand, LimitReason, PlanError, PlannedOperating, PlannedVoltage, PortMode, Preference,
    RequestContext, RequestFlags, RequestMessage, RequestPlan, RequestPlanner, SinkLimits,
};
pub use safety::{PortInputs, PortState, PortSupervisor, SafetyDecision, SafetyTimings};
pub use units::{Milliamps, Millivolts, Milliwatts};

#[cfg(test)]
extern crate std;
