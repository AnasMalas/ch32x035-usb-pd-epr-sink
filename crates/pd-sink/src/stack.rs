//! Typed bridge between this crate's stable policy model and the maintained
//! `usbpd` protocol stack.

use usbpd::protocol_layer::message::data::request::{self, EprRequestDataObject, PowerSource};
use usbpd::protocol_layer::message::data::source_capabilities::SourceCapabilities as StackSourceCapabilities;

use crate::{CapabilitiesKind, CapabilityListError, RequestMessage, RequestPlan, SourceCapabilities, SupplyKind};

/// A validated policy plan could not be represented by the protocol stack.
///
/// These errors indicate an internal integration error rather than a source or
/// user rejection. Plans returned by [`crate::RequestPlanner`] should always
/// convert successfully.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackConversionError {
    /// An EPR Request must repeat the selected Source PDO on the wire.
    MissingEprPdoCopy,
    /// A non-requestable supply kind reached the protocol bridge.
    InvalidSupply(SupplyKind),
}

/// Convert the protocol stack's position-preserving PDO list into the public
/// integer-only capability model.
pub fn capabilities_from_stack(
    capabilities: &StackSourceCapabilities,
) -> Result<SourceCapabilities, CapabilityListError> {
    let count = capabilities.raw_pdos().len();
    if count > crate::capabilities::MAX_SOURCE_PDOS {
        return Err(CapabilityListError::TooMany { supplied: count, maximum: crate::capabilities::MAX_SOURCE_PDOS });
    }

    let kind = if capabilities.is_epr_capabilities() { CapabilitiesKind::Epr } else { CapabilitiesKind::Spr };
    SourceCapabilities::new(kind, capabilities.raw_pdos())
}

/// Convert a validated public request plan into the exact stack request type
/// required for the current SPR or EPR mode.
pub fn request_to_stack(plan: RequestPlan) -> Result<PowerSource, StackConversionError> {
    if matches!(plan.message(), RequestMessage::EprRequest) {
        let pdo = plan.pdo_copy().ok_or(StackConversionError::MissingEprPdoCopy)?;
        return Ok(PowerSource::EprRequest(EprRequestDataObject { rdo: plan.rdo(), pdo }));
    }

    let request = match plan.supply() {
        SupplyKind::Fixed => PowerSource::FixedVariableSupply(request::FixedVariableSupply(plan.rdo())),
        SupplyKind::Pps => PowerSource::Pps(request::Pps(plan.rdo())),
        SupplyKind::SprAvs | SupplyKind::EprAvs => PowerSource::Avs(request::Avs(plan.rdo())),
        SupplyKind::ZeroPadding | SupplyKind::Unsupported => {
            return Err(StackConversionError::InvalidSupply(plan.supply()));
        }
    };
    Ok(request)
}
