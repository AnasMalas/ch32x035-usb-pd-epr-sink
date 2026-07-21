#![no_std]
#![forbid(unsafe_code)]

//! Reusable USB Power Delivery sink logic for CH32X035 applications.
//!
//! The default build contains no executor, allocator, or MCU dependency and is
//! testable on a desktop host. The optional `ch32x035` feature adds the
//! pin-agnostic adapter for the MCU's integrated USB-PD PHY; applications still
//! own their executor, GPIO choices, load gate, commands, and diagnostics.

pub mod capabilities;
#[cfg(feature = "ch32x035")]
pub mod ch32x035;
pub mod command;
pub mod contract;
pub mod controller;
pub mod request;
pub mod runtime;
pub mod safety;
pub mod stack;
pub mod units;

pub use capabilities::{
    AdvertisedPdo, CapabilitiesKind, CapabilityListError, PdoError, PdoValidity, SourceCapabilities, SourceSupply,
    SupplyKind, EPR_AVS_STANDARD_MIN_VOLTAGE,
};
#[cfg(feature = "ch32x035")]
pub use ch32x035::{Ch32x035Port, Ch32x035UsbPdDriver, PhyEvent};
pub use command::{parse_command, Command, CommandError};
pub use contract::{ContractError, ContractState, ContractTracker};
pub use controller::{ControllerAction, ControllerConfig, ControllerError, EprState, SinkController, UserRequest};
pub use request::{
    CurrentConfidence, Demand, LimitReason, PlanError, PlannedOperating, PlannedVoltage, PortMode, Preference,
    RequestContext, RequestFlags, RequestMessage, RequestPlan, RequestPlanner, SinkLimits,
};
pub use runtime::{
    CapabilityPlan, HardResetDirection, RequestResult, SinkConfig, SinkConfigError, SinkDevice, SinkEvent,
    SinkPowerDescriptor, SinkRuntime,
};
pub use safety::{PortInputs, PortState, PortSupervisor, SafetyDecision, SafetyTimings};
pub use stack::{capabilities_from_stack, request_to_stack, StackConversionError};
pub use units::{Milliamps, Millivolts, Milliwatts};

#[cfg(test)]
extern crate std;
