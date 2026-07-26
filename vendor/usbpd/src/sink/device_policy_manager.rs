//! The device policy manager (DPM) allows a device to control the policy engine, and be informed about status changes.
//!
//! For example, through the DPM, a device can request certain source capabilities (voltage, current),
//! or renegotiate the power contract.
use core::future::Future;

use crate::protocol_layer::message::data::{
    alert, epr_mode, request, sink_capabilities, source_capabilities, source_info,
};
use crate::protocol_layer::message::extended::{
    pps_status, sink_capabilities_extended::SinkCapabilitiesExtended, status,
};
use crate::units::Power;

/// Events that the device policy manager can send to the policy engine.
#[derive(Debug)]
pub enum Event {
    /// Empty event.
    None,
    /// Request SPR source capabilities.
    RequestSprSourceCapabilities,
    /// Request EPR source capabilities (when already in EPR mode).
    ///
    /// Sends EprGetSourceCap extended control message.
    /// See [8.3.3.8.1]
    RequestEprSourceCapabilities,
    /// Request the Source's present and guaranteed power information.
    RequestSourceInfo,
    /// Request the Port Partner's general Status Data Block.
    RequestStatus,
    /// Request live Programmable Power Supply status.
    RequestPpsStatus,
    /// Enter EPR mode with the specified operational PDP.
    ///
    /// Initiates EPR mode entry sequence (EPR_Mode Enter -> EnterAcknowledged -> EnterSucceeded).
    /// After successful entry, source automatically sends EPR_Source_Capabilities.
    ///
    /// Per USB PD spec 6.4.10, the Data field in EPR_Mode(Enter) shall be set to the
    /// EPR Sink Operational PDP. For example, a 28V × 5A = 140W device should pass 140W.
    ///
    /// See spec Table 8.39: "Steps for Entering EPR Mode (Success)"
    EnterEprMode(Power),
    /// Exit EPR mode (sink-initiated).
    ///
    /// Sends EPR_Mode (Exit) message to source, then waits for Source_Capabilities.
    /// After receiving caps, negotiation proceeds as normal SPR negotiation.
    /// See spec Table 8.46: "Steps for Exiting EPR Mode (Sink Initiated)"
    ExitEprMode,
    /// Request a certain power level.
    RequestPower(request::PowerSource),
}

impl Event {
    /// Construct an EPR entry event from the exact whole-watt value encoded in
    /// the EPR Mode data object.
    pub fn enter_epr_mode_watts(watts: u8) -> Self {
        Self::EnterEprMode(Power::new::<uom::si::power::watt>(u32::from(watts)))
    }
}

/// Source response when a requested contract was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum RequestRejection {
    /// The source rejected the request.
    Reject,
    /// The source asked the sink to retry later.
    Wait,
}

/// Optional status inquiry that did not return its expected Data Block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum StatusQueryKind {
    /// `Get_Status` / Status.
    General,
    /// `Get_PPS_Status` / PPS_Status.
    Pps,
}

/// Non-fatal outcome of an optional status inquiry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum StatusQueryFailure {
    /// Port Partner returned `Not_Supported`.
    NotSupported,
    /// Port Partner returned the legacy `Reject` response.
    Rejected,
    /// Port Partner asked the Sink to retry later.
    Deferred,
    /// SenderResponseTimer expired.
    Timeout,
}

/// Which port initiated the Hard Reset that moved the Sink to its default
/// power state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum HardResetOrigin {
    /// Hard Reset Signaling was detected from the Source while receiving or
    /// transmitting an ordinary message.
    Source,
    /// The Sink policy engine initiated Hard Reset Signaling as recovery.
    Sink,
}

/// Policy-engine condition that caused a Hard Reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum HardResetReason {
    /// Detailed reason tracking is disabled in this build.
    Unspecified = 255,
    /// Hard Reset Signaling was received from the Source.
    SourceSignaled = 0,
    /// Source capabilities were malformed or incompatible with the current mode.
    InvalidSourceCapabilities = 1,
    /// A Soft Reset could not be transmitted successfully.
    SoftResetFailed = 2,
    /// Source_Capabilities did not arrive before SinkWaitCapTimer expired.
    SourceCapabilitiesTimeout = 3,
    /// The Source did not respond to a Request before SenderResponseTimer expired.
    RequestResponseTimeout = 4,
    /// A protocol error occurred while VBUS was transitioning after an accepted Request.
    PowerTransitionFailure = 5,
    /// EPR entry succeeded but EPR_Source_Capabilities did not arrive in time.
    EprCapabilitiesTimeout = 6,
    /// The EPR state sequence became incompatible with the active contract or received data.
    EprProtocolError = 7,
    /// EPR_KeepAlive could not be completed successfully.
    EprKeepAliveFailed = 8,
}

/// Trait for the device policy manager.
///
/// This entity commands the policy engine and enforces device policy.
pub trait DevicePolicyManager {
    /// Inform the device about source capabilities, e.g. after a request.
    fn inform(&mut self, _source_capabilities: &source_capabilities::SourceCapabilities) -> impl Future<Output = ()> {
        async {}
    }

    /// Request a power source.
    ///
    /// Defaults to 5 V at maximum current.
    fn request(
        &mut self,
        source_capabilities: &source_capabilities::SourceCapabilities,
    ) -> impl Future<Output = request::PowerSource> {
        async {
            request::PowerSource::new_fixed(
                request::CurrentRequest::Highest,
                request::VoltageRequest::Safe5V,
                source_capabilities,
            )
            .unwrap()
        }
    }

    /// Notify the device that it shall transition to a new power level.
    ///
    /// The device is informed about the request that was accepted by the source.
    fn transition_power(&mut self, _accepted: &request::PowerSource) -> impl Future<Output = ()> {
        async {}
    }

    /// Notify the device that its most recent request was rejected or deferred.
    fn request_not_accepted(&mut self, _reason: RequestRejection) -> impl Future<Output = ()> {
        async {}
    }

    /// Inform the product about a Source_Info response.
    fn inform_source_info(&mut self, _source_info: &source_info::SourceInfo) -> impl Future<Output = ()> {
        async {}
    }

    /// Inform the product about an Alert from the Port Partner.
    fn inform_alert(&mut self, _alert: &alert::AlertDataObject) -> impl Future<Output = ()> {
        async {}
    }

    /// Inform the product about a general Status response.
    fn inform_status(&mut self, _status: &status::Status) -> impl Future<Output = ()> {
        async {}
    }

    /// Inform the product about a PPS_Status response.
    fn inform_pps_status(&mut self, _status: &pps_status::PpsStatus) -> impl Future<Output = ()> {
        async {}
    }

    /// Report a refused, deferred, or timed-out optional status inquiry.
    fn status_query_failed(
        &mut self,
        _query: StatusQueryKind,
        _failure: StatusQueryFailure,
    ) -> impl Future<Output = ()> {
        async {}
    }

    /// Notify the device that a hard reset has occurred.
    ///
    /// Per USB PD Spec R3.2 Section 8.3.3.3.9, on entry to PE_SNK_Transition_to_default:
    /// - The sink shall transition to default power level (vSafe5V)
    /// - Local hardware should be reset
    /// - Port data role should be set to UFP
    ///
    /// The device should immediately reset its local power state, prepare for
    /// VBUS to return to vSafe5V, and return promptly so the Protocol Layer can
    /// receive during the recovery interval below.
    #[cfg(feature = "hard-reset-reasons")]
    fn hard_reset(&mut self, _origin: HardResetOrigin, _reason: HardResetReason) -> impl Future<Output = ()> {
        async {}
    }

    #[cfg(not(feature = "hard-reset-reasons"))]
    /// Notify the device of a Hard Reset without retaining a detailed cause.
    fn hard_reset(&mut self, _origin: HardResetOrigin) -> impl Future<Output = ()> {
        async {}
    }

    /// Maximum interval in which Source_Capabilities may arrive after Hard
    /// Reset. Returning zero uses the ordinary SinkWaitCapTimer.
    fn hard_reset_recovery_millis(&self) -> u32 {
        0
    }

    /// Notify the product that valid Source_Capabilities ended Hard Reset
    /// recovery.
    fn hard_reset_recovered(&mut self) -> impl Future<Output = ()> {
        async {}
    }

    /// Notify the device that the Type-C connection or VBUS was removed.
    ///
    /// The implementation must immediately invalidate the active contract and
    /// disable any firmware-controlled load path. Hardware cutoff remains the
    /// primary fast path.
    fn detached(&mut self) -> impl Future<Output = ()> {
        async {}
    }

    /// Notify the device that the policy engine is abandoning the current
    /// port session because the protocol/PHY became unusable.
    ///
    /// This is not necessarily a physical detach, but any explicit contract
    /// must be treated as invalid and a firmware-controlled load must be
    /// disabled before the port is restarted.
    fn protocol_lost(&mut self) -> impl Future<Output = ()> {
        async {}
    }

    /// Notify the device that EPR mode entry failed.
    ///
    /// Per USB PD Spec R3.2 Section 8.3.3.26.2.1, when the source responds with
    /// EPR_Mode (Enter Failed), the sink transitions to soft reset. This callback
    /// informs the DPM of the failure reason before the soft reset occurs.
    ///
    /// The failure reasons are defined in Table 6.50 and include:
    /// - Cable not EPR capable
    /// - Source failed to become VCONN source
    /// - EPR capable bit not set in RDO
    /// - Source unable to enter EPR mode (sink may retry later)
    /// - EPR capable bit not set in PDO
    fn epr_mode_entry_failed(&mut self, _reason: epr_mode::DataEnterFailed) -> impl Future<Output = ()> {
        async {}
    }

    /// Get the sink's power capabilities.
    ///
    /// Per USB PD Spec R3.2 Section 6.4.1.6, sinks respond to Get_Sink_Cap messages
    /// with a Sink_Capabilities message containing PDOs describing what power levels
    /// the sink can operate at.
    ///
    /// All sinks shall minimally offer one PDO at vSafe5V. The default implementation
    /// returns a single 5V @ 100mA PDO.
    fn sink_capabilities(&self) -> sink_capabilities::SinkCapabilities {
        // Default: 5V @ 100mA (1A = 100 * 10mA)
        sink_capabilities::SinkCapabilities::new_vsafe5v_only(100)
    }

    /// Get the sink's 24-byte extended capability descriptor.
    ///
    /// EPR entry's Operational PDP is expected to match this descriptor. The
    /// conservative default describes a 5 W, VBUS-powered, non-EPR sink.
    fn sink_capabilities_extended(&self) -> SinkCapabilitiesExtended {
        SinkCapabilitiesExtended::default()
    }

    /// The policy engine gets and evaluates device policy events when ready.
    ///
    /// By default, this is a future that never resolves.
    ///
    /// <div class="warning">
    /// The function must be safe to cancel. To determine whether your own methods are cancellation safe,
    /// look for the location of uses of .await. This is because when an asynchronous method is cancelled,
    /// that always happens at an .await. If your function behaves correctly even if it is restarted while waiting
    /// at an .await, then it is cancellation safe.
    /// </div>
    fn get_event(
        &mut self,
        _source_capabilities: &source_capabilities::SourceCapabilities,
    ) -> impl Future<Output = Event> {
        async { core::future::pending().await }
    }
}
