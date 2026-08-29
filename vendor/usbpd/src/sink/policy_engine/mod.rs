//! Policy engine for the implementation of a sink.
use core::marker::PhantomData;

use embassy_futures::select::{Either, Either3, select, select3};
use usbpd_traits::Driver;

#[cfg(feature = "hard-reset-reasons")]
use super::device_policy_manager::HardResetReason;
use super::device_policy_manager::{
    DevicePolicyManager, HardResetOrigin, RequestRejection, SinkStartup, SoftResetMode, StatusQueryFailure,
    StatusQueryKind,
};
use crate::counters::Counter;
use crate::protocol_layer::message::data::epr_mode::{self, Action};
use crate::protocol_layer::message::data::request::PowerSource;
use crate::protocol_layer::message::data::source_capabilities::SourceCapabilities;
use crate::protocol_layer::message::data::{Data, alert, request};
use crate::protocol_layer::message::extended::extended_control::ExtendedControlMessageType;
use crate::protocol_layer::message::header::{
    ControlMessageType, DataMessageType, ExtendedMessageType, Header, MessageType, SpecificationRevision,
};
use crate::protocol_layer::message::{Payload, extended};
use crate::protocol_layer::{ProtocolError, RxError, SinkProtocolLayer, TxError, TxValidationError};
use crate::sink::device_policy_manager::Event;
use crate::timers::{Timer, TimerType};
use crate::{DataRole, PowerRole, units};

#[cfg(test)]
mod tests;

/// Sink capability
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// The classic mode of PD operation where explicit contracts are negotiaged using SPR (A)PDOs.
    Spr,
    /// A Power Delivery mode of operation where maximum allowable voltage is 48V.
    Epr,
}

#[derive(Debug, Clone, Copy, Default)]
enum Contract {
    #[default]
    Safe5V,
    _Implicit, // FIXME: Only present after fast role swap, yet unsupported. Limited to max. type C current.
    TransitionToExplicit,
    Explicit,
}

macro_rules! hard_reset_state {
    ($reason:expr) => {{
        #[cfg(feature = "hard-reset-reasons")]
        {
            State::HardReset($reason)
        }
        #[cfg(not(feature = "hard-reset-reasons"))]
        {
            State::HardReset
        }
    }};
}

#[cfg(feature = "hard-reset-reasons")]
macro_rules! hard_reset_pattern {
    ($reason:ident) => {
        State::HardReset($reason)
    };
}

#[cfg(not(feature = "hard-reset-reasons"))]
macro_rules! hard_reset_pattern {
    ($reason:ident) => {
        State::HardReset
    };
}

/// Sink states.
#[derive(Debug, Clone)]
enum State {
    // States of the policy engine as given by the specification.
    /// Default state at startup.
    Startup,
    Discovery,
    WaitForCapabilities,
    EvaluateCapabilities(SourceCapabilities),
    SelectCapability(request::PowerSource),
    TransitionSink(request::PowerSource),
    Ready(request::PowerSource),
    SendNotSupported(request::PowerSource),
    SendSoftReset,
    SoftReset,
    #[cfg(feature = "hard-reset-reasons")]
    HardReset(HardResetReason),
    #[cfg(not(feature = "hard-reset-reasons"))]
    HardReset,
    TransitionToDefault,
    /// Give sink capabilities. The Mode indicates whether to send Sink_Capabilities (Spr)
    /// or EPR_Sink_Capabilities (Epr) per spec 8.3.3.3.10.
    GiveSinkCap(Mode, request::PowerSource),
    GiveSinkCapExtended(request::PowerSource),
    GetSourceCap(Mode, request::PowerSource),
    GetSourceInfo(request::PowerSource),
    GetStatus(request::PowerSource),
    GetPpsStatus(request::PowerSource),
    SourceAlert(alert::AlertDataObject, request::PowerSource),

    // EPR states
    EprModeEntry(request::PowerSource, units::Power),
    EprEntryWaitForResponse(request::PowerSource),
    EprWaitForCapabilities(request::PowerSource),
    EprSendExit,
    EprExitReceived(request::PowerSource),
    EprKeepAlive(request::PowerSource),
}

/// A first Message that belongs to an AMS initiated by this Sink. These
/// transitions must wait for the Source's Rp advertisement to be SinkTxOK.
#[derive(Debug, Clone, Copy)]
enum SinkInitiatedAms {
    GetSourceCap(Mode),
    GetSourceInfo,
    GetStatus,
    GetPpsStatus,
    EnterEprMode(units::Power),
    ExitEprMode,
    RequestPower(request::PowerSource),
    EprKeepAlive,
}

/// Implementation of the sink policy engine.
/// See spec, [8.3.3.3]
#[derive(Debug)]
pub struct Sink<DRIVER: Driver, TIMER: Timer, DPM: DevicePolicyManager> {
    device_policy_manager: DPM,
    protocol_layer: SinkProtocolLayer<DRIVER, TIMER>,
    contract: Contract,
    hard_reset_counter: Counter,
    source_capabilities: Option<SourceCapabilities>,
    /// Last request that completed with PS_RDY. This must not be replaced by a
    /// proposed renegotiation until that new request also reaches PS_RDY.
    active_power_source: Option<request::PowerSource>,
    mode: Mode,
    state: State,
    /// Tracks whether a Get_Source_Cap request is pending.
    /// Per USB PD Spec R3.2 Section 8.3.3.3.8, in EPR mode, receiving a
    /// Source_Capabilities message that was not requested via Get_Source_Cap
    /// shall trigger a Hard Reset.
    get_source_cap_pending: bool,
    /// DPM/timer event retained while the Source owns the CC bus via
    /// SinkTxNG. Source-initiated Messages continue to be serviced while this
    /// is pending.
    pending_sink_ams: Option<SinkInitiatedAms>,
    /// A power request received `Wait` and must be replanned by the DPM after
    /// SinkRequestTimer.
    wait_retry_pending: bool,
    /// Absolute PPS Request-maintenance deadline. A relative timer recreated
    /// on every Ready entry can be starved by telemetry or Source traffic.
    pps_refresh_deadline_tick: Option<u32>,
    /// Absolute EPR keep-alive deadline, independent from unrelated AMSs.
    epr_keep_alive_deadline_tick: Option<u32>,
    /// Origin of the Hard Reset currently returning the Sink to default
    /// power. Every path into `TransitionToDefault` sets this first.
    hard_reset_origin: HardResetOrigin,
    /// Policy-engine condition associated with `hard_reset_origin`.
    #[cfg(feature = "hard-reset-reasons")]
    hard_reset_reason: HardResetReason,
    /// Product-selected receive window for the first Source_Capabilities
    /// after Hard Reset. This survives Startup/Discovery so the PHY listens
    /// throughout recovery instead of sleeping before SinkWaitCapTimer.
    hard_reset_recovery_ms: Option<u32>,

    _timer: PhantomData<TIMER>,
}

/// Errors that can occur in the sink policy engine state machine.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    /// The port partner is unresponsive.
    PortPartnerUnresponsive,
    /// The Type-C connection or VBUS was removed.
    Detached,
    /// The PHY repeatedly discarded traffic and the port should be restarted.
    PhyUnstable,
    /// The DPM supplied an EPR operational PDP outside the 1..=240 W wire range.
    InvalidEprOperationalPdp,
    /// The DPM returned a Request message that is incompatible with the
    /// policy engine's current SPR/EPR mode.
    InvalidRequestForMode,
    /// The locally constructed outgoing message failed validation. Retrying
    /// the unchanged policy state cannot make this error succeed.
    InvalidTransmitMessage(TxValidationError),
    /// A protocol error has occured.
    Protocol(ProtocolError),
}

impl From<ProtocolError> for Error {
    fn from(protocol_error: ProtocolError) -> Self {
        match protocol_error {
            ProtocolError::RxError(RxError::Detached) | ProtocolError::TxError(TxError::Detached) => Error::Detached,
            ProtocolError::RxError(RxError::Discarded) | ProtocolError::TxError(TxError::Discarded) => {
                Error::PhyUnstable
            }
            ProtocolError::TxValidation(error) => Error::InvalidTransmitMessage(error),
            other => Error::Protocol(other),
        }
    }
}

impl<DRIVER: Driver, TIMER: Timer, DPM: DevicePolicyManager> Sink<DRIVER, TIMER, DPM> {
    /// Create a fresh protocol layer with initial state.
    fn new_protocol_layer(driver: DRIVER) -> SinkProtocolLayer<DRIVER, TIMER> {
        let header = Header::new_template(DataRole::Ufp, PowerRole::Sink, SpecificationRevision::R3_X);
        SinkProtocolLayer::new(driver, header)
    }

    /// Create a new sink policy engine with a given `driver`.
    pub fn new(driver: DRIVER, device_policy_manager: DPM) -> Self {
        let (state, mode) = match device_policy_manager.startup() {
            SinkStartup::Fresh => (State::Discovery, Mode::Spr),
            SinkStartup::SoftReset(SoftResetMode::Spr) => (State::SendSoftReset, Mode::Spr),
            SinkStartup::SoftReset(SoftResetMode::Epr) => (State::SendSoftReset, Mode::Epr),
        };
        Self {
            device_policy_manager,
            protocol_layer: Self::new_protocol_layer(driver),
            state,
            contract: Default::default(),
            hard_reset_counter: Counter::new(crate::counters::CounterType::HardReset),
            source_capabilities: None,
            active_power_source: None,
            mode,
            get_source_cap_pending: false,
            pending_sink_ams: None,
            wait_retry_pending: false,
            pps_refresh_deadline_tick: None,
            epr_keep_alive_deadline_tick: None,
            hard_reset_origin: HardResetOrigin::Source,
            #[cfg(feature = "hard-reset-reasons")]
            hard_reset_reason: HardResetReason::SourceSignaled,
            hard_reset_recovery_ms: None,
            _timer: PhantomData,
        }
    }

    /// Set a new driver when re-attached.
    pub fn re_attach(&mut self, driver: DRIVER) {
        self.protocol_layer = Self::new_protocol_layer(driver);
        self.restart();
    }

    /// Access the transport so product code can re-arm a physical port after
    /// `run` returns. The policy engine must be restarted before running it
    /// again.
    pub fn driver_mut(&mut self) -> &mut DRIVER {
        self.protocol_layer.driver_mut()
    }

    /// Reset all state associated with a previous attachment while retaining
    /// the driver and device-policy manager.
    pub fn restart(&mut self) {
        self.protocol_layer.reset();
        self.state = State::Discovery;
        self.contract = Contract::Safe5V;
        self.active_power_source = None;
        self.hard_reset_counter.reset();
        self.source_capabilities = None;
        self.mode = Mode::Spr;
        self.get_source_cap_pending = false;
        self.pending_sink_ams = None;
        self.wait_retry_pending = false;
        self.pps_refresh_deadline_tick = None;
        self.epr_keep_alive_deadline_tick = None;
        self.hard_reset_origin = HardResetOrigin::Source;
        #[cfg(feature = "hard-reset-reasons")]
        {
            self.hard_reset_reason = HardResetReason::SourceSignaled;
        }
        self.hard_reset_recovery_ms = None;
    }

    /// Invalidate a not-yet-started attachment after physical disconnect was
    /// observed, notify the DPM, and reset the policy engine for a future
    /// fresh attachment.
    ///
    /// Once [`Self::run`] has returned `Detached`, the DPM has already been
    /// notified and ordinary [`Self::restart`] is sufficient.
    pub fn restart_unstarted_after_detach(&mut self) {
        self.device_policy_manager.detached();
        self.restart();
    }

    /// Invalidate a not-yet-started attachment after a local PHY/protocol
    /// failure, notify the DPM, and reset for a future fresh attachment.
    ///
    /// Once [`Self::run`] has returned another error, the DPM has already been
    /// notified and ordinary [`Self::restart`] is sufficient.
    pub fn restart_unstarted_after_protocol_loss(&mut self) {
        self.device_policy_manager.protocol_lost();
        self.restart();
    }

    /// Run a single step in the policy engine state machine.
    async fn run_step(&mut self) -> Result<(), Error> {
        let result = self.update_state().await;
        if result.is_ok() {
            return Ok(());
        }

        if let Err(Error::Protocol(protocol_error)) = result {
            let new_state = match (&self.mode, &self.state, protocol_error) {
                // Handle when hard reset is signaled by the driver itself.
                (_, _, ProtocolError::RxError(RxError::HardReset) | ProtocolError::TxError(TxError::HardReset)) => {
                    self.hard_reset_origin = HardResetOrigin::Source;
                    numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                        crate::numeric_trace::NumericTraceEventKind::HardReset,
                        crate::numeric_trace::NumericTraceHardResetPhase::Received as u8,
                        crate::numeric_trace::UNAVAILABLE_U8,
                        self.hard_reset_counter.value(),
                        crate::numeric_trace::UNAVAILABLE_U16,
                        0,
                    ));
                    #[cfg(feature = "hard-reset-reasons")]
                    {
                        self.hard_reset_reason = HardResetReason::SourceSignaled;
                    }
                    Some(State::TransitionToDefault)
                }

                // Handle when soft reset is signaled by the driver itself.
                (_, _, ProtocolError::RxError(RxError::SoftReset)) => Some(State::SoftReset),

                // Per spec 6.3.13: If the Soft_Reset Message fails, a Hard Reset shall be initiated.
                // This handles the case where we're trying to send/receive a soft reset and it fails.
                (_, State::SoftReset | State::SendSoftReset, ProtocolError::TransmitRetriesExceeded(_)) => {
                    Some(hard_reset_state!(HardResetReason::SoftResetFailed))
                }

                // Per spec 8.3.3.3.3: SinkWaitCapTimer timeout triggers Hard Reset.
                (_, State::WaitForCapabilities, ProtocolError::RxError(RxError::ReceiveTimeout)) => {
                    Some(hard_reset_state!(HardResetReason::SourceCapabilitiesTimeout))
                }

                // Per spec 8.3.3.3.5: SenderResponseTimer timeout triggers Hard Reset.
                (_, State::SelectCapability(_), ProtocolError::RxError(RxError::ReceiveTimeout)) => {
                    Some(hard_reset_state!(HardResetReason::RequestResponseTimeout))
                }

                // EnterSucceeded was received, but the first EPR Source
                // Capabilities did not arrive in time.
                (_, State::EprWaitForCapabilities(_), ProtocolError::RxError(RxError::ReceiveTimeout)) => {
                    Some(hard_reset_state!(HardResetReason::EprCapabilitiesTimeout))
                }

                // tEnterEPR expiry requires a Soft Reset.
                (_, State::EprEntryWaitForResponse(_), ProtocolError::RxError(RxError::ReceiveTimeout)) => {
                    Some(State::SendSoftReset)
                }

                // Per USB PD Spec R3.2 Section 8.3.3.3.6 and Table 6.72:
                // Any Protocol Error during power transition (PE_SNK_Transition_Sink state)
                // shall trigger a Hard Reset, not a Soft Reset.
                (_, State::TransitionSink(_), _) => Some(hard_reset_state!(HardResetReason::PowerTransitionFailure)),

                // Only genuine keep-alive protocol failures reach this arm:
                // partner Hard/Soft Reset and detach are handled above or by
                // Error::from before a Sink reset is selected.
                (_, State::EprKeepAlive(_), _) => Some(hard_reset_state!(HardResetReason::EprKeepAliveFailed)),

                // Unexpected messages indicate a protocol error and demand a soft reset.
                // Per spec 6.8.1 Table 6.72 (for non-power-transitioning states).
                // Note: This must come AFTER TransitionSink check above.
                (_, _, ProtocolError::UnexpectedMessage) => Some(State::SendSoftReset),

                // A malformed frame is a protocol error, not a reason to retry
                // the identical receive state forever. Recover through the
                // standard Soft Reset path outside a power transition.
                (_, _, ProtocolError::RxError(RxError::ParseError(_))) => Some(State::SendSoftReset),

                // Per spec Table 6.72: Unsupported messages in Ready state get Not_Supported response.
                (_, State::Ready(power_source), ProtocolError::RxError(RxError::UnsupportedMessage)) => {
                    Some(State::SendNotSupported(*power_source))
                }

                // Before a contract exists there is no retained power request
                // for SendNotSupported, so recover unsupported wire traffic
                // with a Soft Reset instead of retrying forever.
                (_, _, ProtocolError::RxError(RxError::UnsupportedMessage)) => Some(State::SendSoftReset),

                // Per spec 6.6.9.1: Transmission failure (no GoodCRC after retries) triggers Soft Reset.
                // Note: If we're in SoftReset/SendSoftReset state, this is caught above and escalates to Hard Reset.
                (_, _, ProtocolError::TransmitRetriesExceeded(_)) => Some(State::SendSoftReset),

                // Unhandled protocol errors - log and continue.
                // Note: Unrequested Source_Capabilities in EPR mode is handled in Ready state
                // by checking get_source_cap_pending flag (per spec 8.3.3.3.8).
                (_, _, error) => {
                    error!("Protocol error {:?} in sink state transition", error);
                    None
                }
            };

            if let Some(state) = new_state {
                self.state = state
            }

            Ok(())
        } else {
            error!("Unrecoverable result {:?} in sink state transition", result);
            result
        }
    }

    /// Run the sink's state machine continuously.
    ///
    /// The loop is only broken for unrecoverable errors, for example if the port partner is unresponsive.
    pub async fn run(&mut self) -> Result<(), Error> {
        loop {
            match self.run_step().await {
                Ok(()) => {}
                Err(Error::Detached) => {
                    self.device_policy_manager.detached();
                    return Err(Error::Detached);
                }
                Err(error) => {
                    self.device_policy_manager.protocol_lost();
                    return Err(error);
                }
            }
        }
    }

    /// Wait for source capabilities message (either Source_Capabilities or EPR_Source_Capabilities).
    ///
    /// Per USB PD Spec R3.2 Section 8.3.3.3.3 (PE_SNK_Wait_for_Capabilities):
    /// - In SPR Mode: Source_Capabilities Message is received
    /// - In EPR Mode: EPR_Source_Capabilities Message is received
    ///
    /// EPR Mode persists through Soft Reset (unlike Hard Reset which exits EPR per spec 6.8.3.2).
    /// Per spec section 6.4.1.2.2, after a Soft Reset while in EPR Mode, the source sends
    /// EPR_Source_Capabilities. Therefore this function must handle both message types.
    async fn wait_for_source_capabilities(
        protocol_layer: &mut SinkProtocolLayer<DRIVER, TIMER>,
        recovery_ms: Option<u32>,
    ) -> Result<SourceCapabilities, Error> {
        let message = protocol_layer.wait_for_source_capabilities(recovery_ms).await?;
        trace!("Source capabilities: {:?}", message);

        let capabilities = match message.payload {
            Some(Payload::Data(Data::SourceCapabilities(caps))) => caps,
            Some(Payload::Extended(extended::Extended::EprSourceCapabilities(pdos))) => SourceCapabilities(pdos),
            _ => return Err(Error::Protocol(ProtocolError::UnexpectedMessage)),
        };

        Ok(capabilities)
    }

    fn capabilities_valid_for_mode(capabilities: &SourceCapabilities, mode: Mode) -> bool {
        capabilities.has_valid_vsafe_5v()
            && match mode {
                Mode::Spr => !capabilities.is_epr_capabilities(),
                Mode::Epr => capabilities.has_valid_epr_length() && !capabilities.has_epr_pdo_in_spr_positions(),
            }
    }

    fn sink_ams_from_event(event: Event) -> Option<SinkInitiatedAms> {
        match event {
            Event::None => None,
            Event::RequestSprSourceCapabilities => Some(SinkInitiatedAms::GetSourceCap(Mode::Spr)),
            Event::RequestEprSourceCapabilities => Some(SinkInitiatedAms::GetSourceCap(Mode::Epr)),
            Event::RequestSourceInfo => Some(SinkInitiatedAms::GetSourceInfo),
            Event::RequestStatus => Some(SinkInitiatedAms::GetStatus),
            Event::RequestPpsStatus => Some(SinkInitiatedAms::GetPpsStatus),
            Event::EnterEprMode(pdp) => Some(SinkInitiatedAms::EnterEprMode(pdp)),
            Event::ExitEprMode => Some(SinkInitiatedAms::ExitEprMode),
            Event::RequestPower(power_source) => Some(SinkInitiatedAms::RequestPower(power_source)),
        }
    }

    fn state_for_sink_ams(ams: SinkInitiatedAms, power_source: request::PowerSource) -> State {
        match ams {
            SinkInitiatedAms::GetSourceCap(mode) => State::GetSourceCap(mode, power_source),
            SinkInitiatedAms::GetSourceInfo => State::GetSourceInfo(power_source),
            SinkInitiatedAms::GetStatus => State::GetStatus(power_source),
            SinkInitiatedAms::GetPpsStatus => State::GetPpsStatus(power_source),
            SinkInitiatedAms::EnterEprMode(pdp) => State::EprModeEntry(power_source, pdp),
            SinkInitiatedAms::ExitEprMode => match power_source {
                PowerSource::EprRequest(epr) if epr.object_position() <= 7 => State::EprSendExit,
                _ => State::Ready(power_source),
            },
            SinkInitiatedAms::RequestPower(request) => State::SelectCapability(request),
            SinkInitiatedAms::EprKeepAlive => State::EprKeepAlive(power_source),
        }
    }

    fn begin_or_defer_sink_ams(&mut self, ams: SinkInitiatedAms, power_source: request::PowerSource) -> State {
        if self.protocol_layer.sink_tx_ok() {
            Self::state_for_sink_ams(ams, power_source)
        } else {
            // Keep the exact command/request object. Re-planning is only
            // necessary if the Source changes its Capabilities; that path
            // clears this pending value and evaluates the retained DPM intent.
            self.pending_sink_ams.get_or_insert(ams);
            State::Ready(power_source)
        }
    }

    fn is_pps(power_source: request::PowerSource) -> bool {
        match power_source {
            PowerSource::Pps(_) => true,
            PowerSource::EprRequest(epr) => {
                let raw = epr.pdo;
                (raw >> 30) & 0x3 == 0x3 && (raw >> 28) & 0x3 == 0
            }
            _ => false,
        }
    }

    fn deadline_after(ticks: u32) -> u32 {
        TIMER::now_128ms_ticks().wrapping_add(ticks)
    }

    #[cfg(feature = "hard-reset-reasons")]
    #[inline(always)]
    fn reported_hard_reset_reason(reason: HardResetReason) -> HardResetReason {
        reason
    }

    fn ensure_periodic_deadlines(&mut self, power_source: request::PowerSource) {
        if Self::is_pps(power_source) {
            if self.pps_refresh_deadline_tick.is_none() {
                self.pps_refresh_deadline_tick = Some(Self::deadline_after(39));
            }
        } else {
            self.pps_refresh_deadline_tick = None;
        }

        if self.mode == Mode::Epr {
            if self.epr_keep_alive_deadline_tick.is_none() {
                self.epr_keep_alive_deadline_tick = Some(Self::deadline_after(3));
            }
        } else {
            self.epr_keep_alive_deadline_tick = None;
        }
    }

    async fn wait_until(deadline_tick: Option<u32>) {
        match deadline_tick {
            Some(deadline_tick) => {
                let remaining = deadline_tick.wrapping_sub(TIMER::now_128ms_ticks());
                let remaining = if remaining > i32::MAX as u32 { 0 } else { remaining };
                TIMER::after_millis(u64::from(remaining) * 128).await;
            }
            None => core::future::pending().await,
        }
    }

    fn handle_ready_message(
        &mut self,
        message: crate::protocol_layer::message::Message,
        power_source: request::PowerSource,
    ) -> State {
        match message.header.message_type() {
            MessageType::Data(DataMessageType::SourceCapabilities) => {
                // A capability change invalidates any already-encoded pending
                // Request. The DPM retains user intent and will re-plan it.
                self.pending_sink_ams = None;
                self.wait_retry_pending = false;
                if self.mode == Mode::Epr && !self.get_source_cap_pending {
                    hard_reset_state!(HardResetReason::EprProtocolError)
                } else {
                    match message.payload {
                        Some(Payload::Data(Data::SourceCapabilities(capabilities))) => {
                            self.get_source_cap_pending = false;
                            if Self::capabilities_valid_for_mode(&capabilities, self.mode) {
                                State::EvaluateCapabilities(capabilities)
                            } else {
                                hard_reset_state!(HardResetReason::InvalidSourceCapabilities)
                            }
                        }
                        _ => State::SendSoftReset,
                    }
                }
            }
            MessageType::Extended(ExtendedMessageType::EprSourceCapabilities) => {
                self.pending_sink_ams = None;
                self.wait_retry_pending = false;
                if let Some(Payload::Extended(extended::Extended::EprSourceCapabilities(pdos))) = message.payload {
                    self.get_source_cap_pending = false;
                    let caps = SourceCapabilities(pdos);

                    if self.mode != Mode::Epr || !Self::capabilities_valid_for_mode(&caps, Mode::Epr) {
                        hard_reset_state!(HardResetReason::InvalidSourceCapabilities)
                    } else {
                        State::EvaluateCapabilities(caps)
                    }
                } else {
                    State::SendSoftReset
                }
            }
            MessageType::Data(DataMessageType::EprMode) => {
                self.pending_sink_ams = None;
                match message.payload {
                    Some(Payload::Data(Data::EprMode(mode)))
                        if self.mode == Mode::Epr && mode.action() == Action::Exit =>
                    {
                        State::EprExitReceived(power_source)
                    }
                    _ => State::SendSoftReset,
                }
            }
            MessageType::Data(DataMessageType::Alert) => match message.payload {
                Some(Payload::Data(Data::Alert(alert))) => State::SourceAlert(alert, power_source),
                _ => State::SendSoftReset,
            },
            MessageType::Control(ControlMessageType::GetSinkCap) => State::GiveSinkCap(Mode::Spr, power_source),
            MessageType::Control(ControlMessageType::GetSinkCapExtended) => State::GiveSinkCapExtended(power_source),
            MessageType::Extended(ExtendedMessageType::ExtendedControl) => {
                if let Some(Payload::Extended(extended::Extended::ExtendedControl(ctrl))) = &message.payload {
                    if ctrl.message_type() == ExtendedControlMessageType::EprGetSinkCap {
                        State::GiveSinkCap(Mode::Epr, power_source)
                    } else {
                        State::SendNotSupported(power_source)
                    }
                } else {
                    State::SendNotSupported(power_source)
                }
            }
            _ => State::SendNotSupported(power_source),
        }
    }

    async fn update_state(&mut self) -> Result<(), Error> {
        let new_state = match &self.state {
            State::Startup => {
                self.contract = Default::default();
                self.active_power_source = None;
                self.pending_sink_ams = None;
                self.wait_retry_pending = false;
                self.pps_refresh_deadline_tick = None;
                self.epr_keep_alive_deadline_tick = None;
                self.protocol_layer.reset();
                self.mode = Mode::Spr;

                State::Discovery
            }
            State::Discovery => {
                self.protocol_layer.wait_for_vbus().await;
                self.source_capabilities = None;

                State::WaitForCapabilities
            }
            State::WaitForCapabilities => {
                let recovery_ms = self.hard_reset_recovery_ms.take();
                let capabilities = Self::wait_for_source_capabilities(&mut self.protocol_layer, recovery_ms).await?;
                let valid_for_mode = Self::capabilities_valid_for_mode(&capabilities, self.mode);
                if valid_for_mode {
                    if recovery_ms.is_some() {
                        self.device_policy_manager.hard_reset_recovered();
                    }
                    State::EvaluateCapabilities(capabilities)
                } else {
                    hard_reset_state!(HardResetReason::InvalidSourceCapabilities)
                }
            }
            State::EvaluateCapabilities(capabilities) => {
                self.pending_sink_ams = None;
                self.wait_retry_pending = false;
                // Sink now knows that it is attached.
                self.device_policy_manager.inform(capabilities);
                self.source_capabilities = Some(capabilities.clone());

                self.hard_reset_counter.reset();

                let request = self.device_policy_manager.request(self.source_capabilities.as_ref().unwrap());

                if (self.mode == Mode::Epr) != matches!(request, PowerSource::EprRequest(_)) {
                    return Err(Error::InvalidRequestForMode);
                }

                State::SelectCapability(request)
            }
            State::SelectCapability(power_source) => {
                // Any power request now being attempted supersedes an older
                // scheduled retry. A new Wait response below arms it again.
                self.wait_retry_pending = false;
                self.protocol_layer.request_power(*power_source).await?;

                let message_type = self
                    .protocol_layer
                    .receive_message_type(
                        &[
                            MessageType::Control(ControlMessageType::Accept),
                            MessageType::Control(ControlMessageType::Wait),
                            MessageType::Control(ControlMessageType::Reject),
                        ],
                        TimerType::SenderResponse,
                    )
                    .await?
                    .header
                    .message_type();

                let MessageType::Control(control_message_type) = message_type else { unreachable!() };

                match control_message_type {
                    ControlMessageType::Reject => {
                        self.device_policy_manager.request_not_accepted(RequestRejection::Reject);
                    }
                    ControlMessageType::Wait => {
                        self.device_policy_manager.request_not_accepted(RequestRejection::Wait);
                        self.wait_retry_pending = true;
                    }
                    ControlMessageType::Accept => {}
                    _ => unreachable!(),
                }

                match (self.contract, control_message_type) {
                    (_, ControlMessageType::Accept) => State::TransitionSink(*power_source),
                    (Contract::Safe5V, ControlMessageType::Wait | ControlMessageType::Reject) => {
                        State::WaitForCapabilities
                    }
                    (Contract::Explicit, ControlMessageType::Reject)
                        if self.mode == Mode::Epr
                            && !matches!(self.active_power_source, Some(PowerSource::EprRequest(_))) =>
                    {
                        hard_reset_state!(HardResetReason::EprProtocolError)
                    }
                    (Contract::Explicit, ControlMessageType::Reject) => {
                        State::Ready(self.active_power_source.expect("explicit contract has an active request"))
                    }
                    (Contract::Explicit, ControlMessageType::Wait) => {
                        // Per spec 8.3.3.3.7: On entry to Ready as result of Wait,
                        // initialize and run SinkRequestTimer.
                        State::Ready(self.active_power_source.expect("explicit contract has an active request"))
                    }
                    _ => unreachable!(),
                }
            }
            State::TransitionSink(power_source) => {
                let accepted_power_source = *power_source;
                self.protocol_layer
                    .receive_message_type(
                        &[MessageType::Control(ControlMessageType::PsRdy)],
                        match self.mode {
                            Mode::Epr => TimerType::PSTransitionEpr,
                            Mode::Spr => TimerType::PSTransitionSpr,
                        },
                    )
                    .await?;

                self.contract = Contract::TransitionToExplicit;
                self.device_policy_manager.transition_power(&accepted_power_source);
                self.active_power_source = Some(accepted_power_source);
                if Self::is_pps(accepted_power_source) {
                    self.pps_refresh_deadline_tick = Some(Self::deadline_after(39));
                } else {
                    self.pps_refresh_deadline_tick = None;
                }
                // A successful Request/PS_RDY exchange is current EPR traffic.
                // Rearm from PS_RDY so a keep-alive deadline that expired during
                // a long high-to-low VBUS transition cannot fire immediately.
                self.epr_keep_alive_deadline_tick = None;
                self.ensure_periodic_deadlines(accepted_power_source);
                State::Ready(accepted_power_source)
            }
            State::Ready(power_source) => {
                let active_power_source = *power_source;
                self.ensure_periodic_deadlines(active_power_source);
                // TODO: Entry: Init. and run DiscoverIdentityTimer(4)
                // TODO: Entry: Send GetSinkCap message if sink supports fast role swap
                // Sink-initiated AMSs are held below until Rp is SinkTxOK.
                //
                // Timers implemented:
                // - SinkRequestTimer: Per spec 8.3.3.3.7, after receiving Wait, wait tSinkRequest
                //   before allowing re-request. On timeout, transition to SelectCapability.
                // - SinkPPSPeriodicTimer: triggers SelectCapability in SPR PPS mode
                // - SinkEPRKeepAliveTimer: triggers EprKeepAlive in EPR mode
                self.contract = Contract::Explicit;

                if let Some(pending) = self.pending_sink_ams.take() {
                    if self.protocol_layer.sink_tx_ok() {
                        Self::state_for_sink_ams(pending, active_power_source)
                    } else {
                        // While the Source owns CC, continue servicing its AMS
                        // and periodically re-sample Rp without dropping the
                        // exact user/timer request that is waiting.
                        self.pending_sink_ams = Some(pending);
                        match select(self.protocol_layer.receive_message(), TIMER::after_millis(1)).await {
                            Either::First(message) => self.handle_ready_message(message?, active_power_source),
                            Either::Second(()) => State::Ready(active_power_source),
                        }
                    }
                } else {
                    let receive_fut = self.protocol_layer.receive_message();
                    let event_fut = self.device_policy_manager.get_event(self.source_capabilities.as_ref().unwrap());
                    let pps_periodic_fut = Self::wait_until(self.pps_refresh_deadline_tick);
                    let epr_keep_alive_fut = Self::wait_until(self.epr_keep_alive_deadline_tick);
                    // Per spec 8.3.3.3.7: SinkRequestTimer runs concurrently when re-entering
                    // Ready after a Wait response. On timeout, transition to SelectCapability.
                    // Per spec 6.6.4.1: Ensures minimum tSinkRequest (100ms) delay before re-request.
                    let retry_waiting = self.wait_retry_pending;
                    let sink_request_fut = async move {
                        if retry_waiting {
                            TimerType::get_timer::<TIMER>(TimerType::SinkRequest).await
                        } else {
                            core::future::pending().await
                        }
                    };
                    let timers_fut = async { select3(pps_periodic_fut, epr_keep_alive_fut, sink_request_fut).await };

                    match select3(receive_fut, event_fut, timers_fut).await {
                        Either3::First(message) => self.handle_ready_message(message?, active_power_source),
                        Either3::Second(event) => match Self::sink_ams_from_event(event) {
                            Some(ams) => self.begin_or_defer_sink_ams(ams, active_power_source),
                            None => State::Ready(active_power_source),
                        },
                        Either3::Third(timeout_source) => {
                            let pps_timeout = matches!(&timeout_source, Either3::First(_));
                            let ams = match timeout_source {
                                Either3::Second(_) => {
                                    self.epr_keep_alive_deadline_tick = None;
                                    SinkInitiatedAms::EprKeepAlive
                                }
                                Either3::First(_) | Either3::Third(_) => {
                                    if pps_timeout {
                                        self.pps_refresh_deadline_tick = None;
                                    } else {
                                        self.wait_retry_pending = false;
                                    }
                                    // PPS refreshes and Wait retries must both
                                    // pass through product policy. Besides
                                    // re-planning against the latest retained
                                    // limits, this lets the DPM re-arm its
                                    // contract tracker before Accept/PS_RDY.
                                    let retry =
                                        self.device_policy_manager.request(self.source_capabilities.as_ref().unwrap());
                                    if (self.mode == Mode::Epr) != matches!(retry, PowerSource::EprRequest(_)) {
                                        return Err(Error::InvalidRequestForMode);
                                    }
                                    SinkInitiatedAms::RequestPower(retry)
                                }
                            };
                            self.begin_or_defer_sink_ams(ams, active_power_source)
                        }
                    }
                }
            }
            State::SendNotSupported(power_source) => {
                self.protocol_layer.transmit_control_message(ControlMessageType::NotSupported).await?;

                State::Ready(*power_source)
            }
            State::SendSoftReset => {
                self.pending_sink_ams = None;
                self.wait_retry_pending = false;
                self.protocol_layer.reset();

                self.protocol_layer.transmit_control_message(ControlMessageType::SoftReset).await?;

                self.protocol_layer
                    .receive_message_type(
                        &[MessageType::Control(ControlMessageType::Accept)],
                        TimerType::SenderResponse,
                    )
                    .await?;

                State::WaitForCapabilities
            }
            State::SoftReset => {
                self.pending_sink_ams = None;
                self.wait_retry_pending = false;
                self.protocol_layer.reset();

                // The Accept response to Soft_Reset is the first message in
                // the new protocol session and therefore uses MessageID zero.
                self.protocol_layer.transmit_control_message(ControlMessageType::Accept).await?;

                State::WaitForCapabilities
            }
            hard_reset_pattern!(reason) => {
                self.pending_sink_ams = None;
                self.wait_retry_pending = false;
                self.pps_refresh_deadline_tick = None;
                self.epr_keep_alive_deadline_tick = None;
                // Per USB PD Spec R3.2 Section 8.3.3.3.8 (PE_SNK_Hard_Reset):
                // Entry conditions:
                // - PSTransitionTimer timeout (when HardResetCounter <= nHardResetCount)
                // - Hard reset request from Device Policy Manager
                // - EPR mode and EPR_Source_Capabilities message with EPR PDO in pos. 1..7
                // - Source_Capabilities message not requested by Get_Source_Cap
                // - SinkWaitCapTimer timeout (when HardResetCounter <= nHardResetCount)
                //
                // On entry: Request Hard Reset Signaling AND increment HardResetCounter

                // Increment counter first - returns Err when counter > nHardResetCount.
                // Per spec 8.3.3.3.8: If HardResetCounter > nHardResetCount (> 2),
                // the Sink shall assume that the Source is non-responsive.
                // With counter max_value = 3, we allow 3 hard reset attempts (counter 1, 2, 3)
                // before wrap returns Err.
                if self.hard_reset_counter.increment().is_err() {
                    return Err(Error::PortPartnerUnresponsive);
                }

                #[cfg(feature = "numeric-trace")]
                let trace_reason = {
                    #[cfg(feature = "hard-reset-reasons")]
                    {
                        u16::from(*reason as u8)
                    }
                    #[cfg(not(feature = "hard-reset-reasons"))]
                    {
                        crate::numeric_trace::UNAVAILABLE_U16
                    }
                };

                numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                    crate::numeric_trace::NumericTraceEventKind::HardReset,
                    crate::numeric_trace::NumericTraceHardResetPhase::TransmitStart as u8,
                    crate::numeric_trace::UNAVAILABLE_U8,
                    self.hard_reset_counter.value(),
                    crate::numeric_trace::UNAVAILABLE_U16,
                    trace_reason,
                ));

                // Transmit Hard Reset Signaling
                self.hard_reset_origin = HardResetOrigin::Sink;
                #[cfg(feature = "hard-reset-reasons")]
                {
                    self.hard_reset_reason = *reason;
                }
                #[cfg(not(feature = "numeric-trace"))]
                self.protocol_layer.hard_reset().await?;
                #[cfg(feature = "numeric-trace")]
                {
                    let hard_reset_result = self.protocol_layer.hard_reset().await;
                    numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                        crate::numeric_trace::NumericTraceEventKind::HardReset,
                        if hard_reset_result.is_ok() {
                            crate::numeric_trace::NumericTraceHardResetPhase::TransmitComplete as u8
                        } else {
                            crate::numeric_trace::NumericTraceHardResetPhase::TransmitFailure as u8
                        },
                        crate::numeric_trace::UNAVAILABLE_U8,
                        self.hard_reset_counter.value(),
                        crate::numeric_trace::UNAVAILABLE_U16,
                        trace_reason,
                    ));
                    hard_reset_result?;
                }

                State::TransitionToDefault
            }
            State::TransitionToDefault => {
                self.pending_sink_ams = None;
                self.wait_retry_pending = false;
                self.pps_refresh_deadline_tick = None;
                self.epr_keep_alive_deadline_tick = None;
                // Per USB PD Spec R3.2 Section 8.3.3.3.9 (PE_SNK_Transition_to_default):
                // This state is entered when:
                // - Hard Reset Signaling is detected (received or transmitted)
                // - From PE_SNK_Hard_Reset after hard reset is complete
                //
                // On entry:
                // - Indicate to DPM that Sink shall transition to default
                // - Request reset of local hardware
                // - Request DPM that Port Data Role is set to UFP
                //
                // Transition to PE_SNK_Startup when:
                // - DPM indicates Sink has reached default level

                // Arm the product-selected recovery receive window before
                // notifying the DPM. The DPM must return promptly so the PHY
                // is listening while the Source returns to default power.
                let recovery_ms = self.device_policy_manager.hard_reset_recovery_millis();
                self.hard_reset_recovery_ms = (recovery_ms != 0).then_some(recovery_ms);

                // Notify DPM about hard reset (DPM should transition to default power level).
                #[cfg(feature = "hard-reset-reasons")]
                {
                    self.device_policy_manager
                        .hard_reset(self.hard_reset_origin, Self::reported_hard_reset_reason(self.hard_reset_reason));
                }
                #[cfg(not(feature = "hard-reset-reasons"))]
                {
                    self.device_policy_manager.hard_reset(self.hard_reset_origin);
                }

                // Reset protocol layer (per spec 6.8.3: "Protocol Layers shall be reset as for Soft Reset")
                self.protocol_layer.reset();

                // Reset EPR mode (per spec 6.8.3.2: "Hard Reset shall cause EPR Mode to be exited")
                self.mode = Mode::Spr;

                // Reset contract to default
                self.contract = Contract::Safe5V;
                self.active_power_source = None;

                // Clear cached source capabilities
                self.source_capabilities = None;

                State::Startup
            }
            State::GiveSinkCap(response_mode, power_source) => {
                // Per USB PD Spec R3.2 Section 8.3.3.3.10:
                // - Send Sink_Capabilities when Get_Sink_Cap was received
                // - Send EPR_Sink_Capabilities when EPR_Get_Sink_Cap was received
                let sink_caps = self.device_policy_manager.sink_capabilities();
                match response_mode {
                    Mode::Spr => {
                        self.protocol_layer.transmit_sink_capabilities(sink_caps).await?;
                    }
                    Mode::Epr => {
                        self.protocol_layer.transmit_epr_sink_capabilities(sink_caps).await?;
                    }
                }

                State::Ready(*power_source)
            }
            State::GiveSinkCapExtended(power_source) => {
                let capabilities = self.device_policy_manager.sink_capabilities_extended();
                self.protocol_layer.transmit_sink_capabilities_extended(capabilities).await?;
                State::Ready(*power_source)
            }
            State::GetSourceCap(requested_mode, power_source) => {
                // Per USB PD Spec R3.2 Section 8.3.3.3.12 (PE_SNK_Get_Source_Cap):
                // - Send Get_Source_Cap (SPR) or EPR_Get_Source_Cap (EPR)
                // - Start SenderResponseTimer
                // - On timeout or mode mismatch → Ready
                // - On matching capabilities received → EvaluateCapabilities
                //
                // Set flag before sending to track that we requested source capabilities.
                // Per spec 8.3.3.3.8, in EPR mode, receiving an unrequested
                // Source_Capabilities message triggers a Hard Reset.
                self.get_source_cap_pending = true;

                match requested_mode {
                    Mode::Spr => {
                        self.protocol_layer.transmit_control_message(ControlMessageType::GetSourceCap).await?;
                    }
                    Mode::Epr => {
                        self.protocol_layer
                            .transmit_extended_control_message(
                                crate::protocol_layer::message::extended::extended_control::ExtendedControlMessageType::EprGetSourceCap,
                            )
                            .await?;
                    }
                };

                // Per spec 8.3.3.3.12: Use SenderResponseTimer (not SinkWaitCap)
                let result = self
                    .protocol_layer
                    .receive_message_type(
                        &[
                            MessageType::Data(DataMessageType::SourceCapabilities),
                            MessageType::Extended(ExtendedMessageType::EprSourceCapabilities),
                        ],
                        TimerType::SenderResponse,
                    )
                    .await;

                self.get_source_cap_pending = false;

                // Per spec 8.3.3.3.12: On timeout, inform DPM and transition to Ready
                let message = match result {
                    Ok(msg) => msg,
                    Err(ProtocolError::RxError(RxError::ReceiveTimeout)) => {
                        // Inform DPM of timeout (no capabilities received)
                        warn!("Get_Source_Cap timeout, returning to Ready");
                        self.state = State::Ready(*power_source);
                        return Ok(());
                    }
                    Err(e) => return Err(e.into()),
                };

                // Per spec 8.3.3.3.12:
                // - In SPR mode + SPR caps requested + Source_Capabilities received → EvaluateCapabilities
                // - In EPR mode + EPR caps requested + EPR_Source_Capabilities received → EvaluateCapabilities
                // - Mode mismatch (e.g., EPR mode but SPR caps requested) → Ready
                let received_spr =
                    matches!(message.header.message_type(), MessageType::Data(DataMessageType::SourceCapabilities));
                let received_epr = matches!(
                    message.header.message_type(),
                    MessageType::Extended(ExtendedMessageType::EprSourceCapabilities)
                );

                let mode_matches = (*requested_mode == Mode::Spr && self.mode == Mode::Spr && received_spr)
                    || (*requested_mode == Mode::Epr && self.mode == Mode::Epr && received_epr);

                // A partner can use a capability message type with a payload
                // shape that does not match it (for example, a reserved PDO
                // encoding). Treat that as malformed wire traffic instead of
                // assuming the parser produced typed capabilities.
                let capabilities = match message.payload {
                    Some(Payload::Data(Data::SourceCapabilities(capabilities))) => Some(capabilities),
                    Some(Payload::Extended(extended::Extended::EprSourceCapabilities(pdos))) => {
                        Some(SourceCapabilities(pdos))
                    }
                    _ => None,
                };

                match capabilities {
                    Some(capabilities)
                        if mode_matches && Self::capabilities_valid_for_mode(&capabilities, self.mode) =>
                    {
                        State::EvaluateCapabilities(capabilities)
                    }
                    Some(_) if mode_matches => hard_reset_state!(HardResetReason::InvalidSourceCapabilities),
                    Some(_) => State::Ready(*power_source),
                    None => State::SendSoftReset,
                }
            }
            State::GetSourceInfo(power_source) => {
                self.protocol_layer.transmit_control_message(ControlMessageType::GetSourceInfo).await?;

                let response = self
                    .protocol_layer
                    .receive_message_type(
                        &[
                            MessageType::Data(DataMessageType::SourceInfo),
                            MessageType::Control(ControlMessageType::NotSupported),
                            // Some otherwise usable sources answer optional
                            // queries with the older generic refusal or ask
                            // the Sink to try later. Neither response may
                            // disturb the existing explicit contract.
                            MessageType::Control(ControlMessageType::Reject),
                            MessageType::Control(ControlMessageType::Wait),
                        ],
                        TimerType::SenderResponse,
                    )
                    .await;

                match response {
                    Ok(message) => {
                        if let Some(Payload::Data(Data::SourceInfo(source_info))) = message.payload {
                            self.device_policy_manager.inform_source_info(&source_info);
                        }
                    }
                    Err(
                        error @ ProtocolError::RxError(RxError::Detached | RxError::HardReset | RxError::SoftReset),
                    )
                    | Err(error @ ProtocolError::TxError(TxError::Detached | TxError::HardReset)) => {
                        return Err(error.into());
                    }
                    // Source_Info refines product reporting but is not needed
                    // to preserve an existing explicit contract.
                    Err(_) => {}
                }

                State::Ready(*power_source)
            }
            State::SourceAlert(alert, power_source) => {
                self.device_policy_manager.inform_alert(alert);
                State::Ready(*power_source)
            }
            State::GetStatus(power_source) => {
                self.protocol_layer.transmit_control_message(ControlMessageType::GetStatus).await?;

                let response = self
                    .protocol_layer
                    .receive_message_type(
                        &[
                            MessageType::Extended(ExtendedMessageType::Status),
                            MessageType::Control(ControlMessageType::NotSupported),
                            MessageType::Control(ControlMessageType::Reject),
                            MessageType::Control(ControlMessageType::Wait),
                        ],
                        TimerType::SenderResponse,
                    )
                    .await;

                match response {
                    Ok(message) => match message.payload {
                        Some(Payload::Extended(extended::Extended::Status(status))) => {
                            self.device_policy_manager.inform_status(&status);
                        }
                        None => {
                            let failure = match message.header.message_type() {
                                MessageType::Control(ControlMessageType::NotSupported) => {
                                    Some(StatusQueryFailure::NotSupported)
                                }
                                MessageType::Control(ControlMessageType::Reject) => Some(StatusQueryFailure::Rejected),
                                MessageType::Control(ControlMessageType::Wait) => Some(StatusQueryFailure::Deferred),
                                _ => None,
                            };
                            if let Some(failure) = failure {
                                self.device_policy_manager.status_query_failed(StatusQueryKind::General, failure);
                            }
                        }
                        _ => return Err(Error::Protocol(ProtocolError::UnexpectedMessage)),
                    },
                    Err(ProtocolError::RxError(RxError::ReceiveTimeout)) => {
                        self.device_policy_manager
                            .status_query_failed(StatusQueryKind::General, StatusQueryFailure::Timeout);
                    }
                    Err(error) => return Err(error.into()),
                }

                State::Ready(*power_source)
            }
            State::GetPpsStatus(power_source) => {
                self.protocol_layer.transmit_control_message(ControlMessageType::GetPpsStatus).await?;

                let response = self
                    .protocol_layer
                    .receive_message_type(
                        &[
                            MessageType::Extended(ExtendedMessageType::PpsStatus),
                            MessageType::Control(ControlMessageType::NotSupported),
                            MessageType::Control(ControlMessageType::Reject),
                            MessageType::Control(ControlMessageType::Wait),
                        ],
                        TimerType::SenderResponse,
                    )
                    .await;

                match response {
                    Ok(message) => match message.payload {
                        Some(Payload::Extended(extended::Extended::PpsStatus(status))) => {
                            self.device_policy_manager.inform_pps_status(&status);
                        }
                        None => {
                            let failure = match message.header.message_type() {
                                MessageType::Control(ControlMessageType::NotSupported) => {
                                    Some(StatusQueryFailure::NotSupported)
                                }
                                MessageType::Control(ControlMessageType::Reject) => Some(StatusQueryFailure::Rejected),
                                MessageType::Control(ControlMessageType::Wait) => Some(StatusQueryFailure::Deferred),
                                _ => None,
                            };
                            if let Some(failure) = failure {
                                self.device_policy_manager.status_query_failed(StatusQueryKind::Pps, failure);
                            }
                        }
                        _ => return Err(Error::Protocol(ProtocolError::UnexpectedMessage)),
                    },
                    Err(ProtocolError::RxError(RxError::ReceiveTimeout)) => {
                        self.device_policy_manager
                            .status_query_failed(StatusQueryKind::Pps, StatusQueryFailure::Timeout);
                    }
                    Err(error) => return Err(error.into()),
                }

                State::Ready(*power_source)
            }
            State::EprModeEntry(power_source, operational_pdp) => {
                // Request entry into EPR mode.
                // Per spec 8.3.3.26.2.1 (PE_SNK_Send_EPR_Mode_Entry), sink sends EPR_Mode (Enter)
                // and starts SenderResponseTimer and SinkEPREnterTimer.
                //
                // Per spec 6.4.10, the Data field shall be set to the EPR Sink Operational PDP.
                //
                // Note: The spec says SinkEPREnterTimer (500ms) should run continuously across
                // both EprModeEntry and EprEntryWaitForResponse states until stopped or timeout.
                // Our implementation uses SenderResponseTimer (30ms) here and a fresh
                // SinkEPREnterTimer (500ms) in EprEntryWaitForResponse. This means the total
                // timeout could be ~530ms instead of 500ms in edge cases. However, this is
                // within the spec's allowed range (tEnterEPR max = 550ms per Table 6.71).
                let Some(pdp_watts) = operational_pdp.as_watts_floor() else {
                    return Err(Error::InvalidEprOperationalPdp);
                };
                if !(1..=240).contains(&pdp_watts) {
                    return Err(Error::InvalidEprOperationalPdp);
                }
                self.protocol_layer.transmit_epr_mode(Action::Enter, pdp_watts).await?;

                // Wait for EnterAcknowledged with SenderResponseTimer (spec step 9-14)
                let message = self
                    .protocol_layer
                    .receive_message_type(&[MessageType::Data(DataMessageType::EprMode)], TimerType::SenderResponse)
                    .await?;

                match message.payload {
                    Some(Payload::Data(Data::EprMode(epr_mode))) => match epr_mode.action() {
                        Action::EnterAcknowledged => {
                            // Source acknowledged, now wait for EnterSucceeded
                            State::EprEntryWaitForResponse(*power_source)
                        }
                        Action::EnterSucceeded => {
                            // EnterAcknowledged is mandatory as the first response.
                            State::SendSoftReset
                        }
                        Action::Exit => State::EprExitReceived(*power_source),
                        Action::EnterFailed => {
                            // Per spec 8.3.3.26.2.1: EnterFailed → Soft Reset
                            // Notify DPM of the failure reason before soft reset
                            let reason = epr_mode::DataEnterFailed::from(epr_mode.data());
                            self.device_policy_manager.epr_mode_entry_failed(reason);
                            State::SendSoftReset
                        }
                        // Per spec 8.3.3.26.2.1: any other EPR_Mode message → Soft Reset
                        _ => State::SendSoftReset,
                    },
                    _ => State::SendSoftReset,
                }
            }
            State::EprEntryWaitForResponse(power_source) => {
                // Wait for EnterSucceeded after receiving EnterAcknowledged.
                // Per spec 8.3.3.26.2.2 (PE_SNK_EPR_Mode_Wait_For_Response), use SinkEPREnterTimer
                // for the overall timeout while source performs cable discovery.
                let message = self
                    .protocol_layer
                    .receive_message_type(&[MessageType::Data(DataMessageType::EprMode)], TimerType::SinkEPREnter)
                    .await?;

                match message.payload {
                    Some(Payload::Data(Data::EprMode(epr_mode))) => match epr_mode.action() {
                        Action::EnterSucceeded => {
                            // EPR mode entry succeeded. Per spec Table 8.39 step 21-29,
                            // source will automatically send EPR_Source_Capabilities after this.
                            self.mode = Mode::Epr;
                            State::EprWaitForCapabilities(*power_source)
                        }
                        Action::Exit => State::EprExitReceived(*power_source),
                        Action::EnterFailed => {
                            // Per spec 8.3.3.26.2.2: EnterFailed → Soft Reset
                            // Notify DPM of the failure reason before soft reset
                            let reason = epr_mode::DataEnterFailed::from(epr_mode.data());
                            self.device_policy_manager.epr_mode_entry_failed(reason);
                            State::SendSoftReset
                        }
                        // Per spec 8.3.3.26.2.2: any other EPR_Mode message → Soft Reset
                        _ => State::SendSoftReset,
                    },
                    _ => State::SendSoftReset,
                }
            }
            State::EprWaitForCapabilities(_power_source) => {
                // After successful EPR mode entry, source automatically sends EPR_Source_Capabilities.
                // This may be a chunked extended message that requires assembly.
                // Wait for the capabilities and evaluate them.
                let message = self.protocol_layer.wait_for_source_capabilities(None).await?;

                match message.payload {
                    Some(Payload::Data(Data::SourceCapabilities(_))) => {
                        hard_reset_state!(HardResetReason::EprProtocolError)
                    }
                    Some(Payload::Extended(extended::Extended::EprSourceCapabilities(pdos))) => {
                        let capabilities = SourceCapabilities(pdos);
                        if Self::capabilities_valid_for_mode(&capabilities, Mode::Epr) {
                            State::EvaluateCapabilities(capabilities)
                        } else {
                            hard_reset_state!(HardResetReason::InvalidSourceCapabilities)
                        }
                    }
                    _ => {
                        error!("Expected source capabilities after EPR mode entry");
                        hard_reset_state!(HardResetReason::EprProtocolError)
                    }
                }
            }
            State::EprSendExit => {
                // Inform partner we are exiting EPR.
                self.protocol_layer.transmit_epr_mode(Action::Exit, 0).await?;
                self.mode = Mode::Spr;
                State::WaitForCapabilities
            }
            State::EprExitReceived(power_source) => {
                // Per USB PD Spec R3.2 Section 8.3.3.26.4.2 (PE_SNK_EPR_Mode_Exit_Received):
                // - If in an Explicit Contract with an SPR (A)PDO → WaitForCapabilities
                // - If NOT in an Explicit Contract with an SPR (A)PDO → HardReset
                //
                // SPR PDOs are in object positions 1-7, EPR PDOs are in positions 8+.
                // In EPR mode, requests use EprRequest which contains the RDO with object position.
                self.pending_sink_ams = None;
                self.wait_retry_pending = false;
                self.pps_refresh_deadline_tick = None;
                self.epr_keep_alive_deadline_tick = None;
                self.mode = Mode::Spr;

                let is_epr_pdo_contract = match power_source {
                    PowerSource::EprRequest(epr) => {
                        // Extract object position from RDO (bits 28-31)
                        epr.object_position() >= 8
                    }
                    // Non-EprRequest variants are only used in SPR mode, so always SPR PDOs
                    _ => false,
                };

                if is_epr_pdo_contract {
                    hard_reset_state!(HardResetReason::EprProtocolError)
                } else {
                    State::WaitForCapabilities
                }
            }
            State::EprKeepAlive(power_source) => {
                // Per spec 8.3.3.3.11 (PE_SNK_EPR_Keep_Alive):
                // - Entry: Send EPR_KeepAlive message, start SenderResponseTimer
                // - On EPR_KeepAlive_Ack: transition to Ready (which restarts SinkEPRKeepAliveTimer)
                // - On timeout: transition to HardReset
                numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                    crate::numeric_trace::NumericTraceEventKind::EprKeepAlive,
                    crate::numeric_trace::NumericTraceEprKeepAlivePhase::Request as u8,
                    crate::numeric_trace::UNAVAILABLE_U8,
                    crate::numeric_trace::UNAVAILABLE_U8,
                    crate::numeric_trace::UNAVAILABLE_U16,
                    crate::numeric_trace::UNAVAILABLE_U16,
                ));
                #[cfg(not(feature = "numeric-trace"))]
                self.protocol_layer
                    .transmit_extended_control_message(
                        crate::protocol_layer::message::extended::extended_control::ExtendedControlMessageType::EprKeepAlive,
                    )
                    .await?;
                #[cfg(feature = "numeric-trace")]
                {
                    let transmit_result = self
                        .protocol_layer
                        .transmit_extended_control_message(
                            crate::protocol_layer::message::extended::extended_control::ExtendedControlMessageType::EprKeepAlive,
                        )
                        .await;
                    if transmit_result.is_err() {
                        numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                            crate::numeric_trace::NumericTraceEventKind::EprKeepAlive,
                            crate::numeric_trace::NumericTraceEprKeepAlivePhase::ProtocolFailure as u8,
                            crate::numeric_trace::UNAVAILABLE_U8,
                            crate::numeric_trace::UNAVAILABLE_U8,
                            crate::numeric_trace::UNAVAILABLE_U16,
                            crate::numeric_trace::UNAVAILABLE_U16,
                        ));
                    }
                    transmit_result?;
                }
                #[cfg(not(feature = "numeric-trace"))]
                let message = self
                    .protocol_layer
                    .receive_message_type(
                        &[MessageType::Extended(ExtendedMessageType::ExtendedControl)],
                        TimerType::SenderResponse,
                    )
                    .await?;
                #[cfg(feature = "numeric-trace")]
                let message = {
                    let response = self
                        .protocol_layer
                        .receive_message_type(
                            &[MessageType::Extended(ExtendedMessageType::ExtendedControl)],
                            TimerType::SenderResponse,
                        )
                        .await;
                    match response {
                        Ok(message) => message,
                        Err(ProtocolError::RxError(RxError::ReceiveTimeout)) => {
                            numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                                crate::numeric_trace::NumericTraceEventKind::EprKeepAlive,
                                crate::numeric_trace::NumericTraceEprKeepAlivePhase::Timeout as u8,
                                crate::numeric_trace::UNAVAILABLE_U8,
                                crate::numeric_trace::UNAVAILABLE_U8,
                                crate::numeric_trace::UNAVAILABLE_U16,
                                crate::numeric_trace::UNAVAILABLE_U16,
                            ));
                            return Err(ProtocolError::RxError(RxError::ReceiveTimeout).into());
                        }
                        Err(error) => {
                            numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                                crate::numeric_trace::NumericTraceEventKind::EprKeepAlive,
                                crate::numeric_trace::NumericTraceEprKeepAlivePhase::ProtocolFailure as u8,
                                crate::numeric_trace::UNAVAILABLE_U8,
                                crate::numeric_trace::UNAVAILABLE_U8,
                                crate::numeric_trace::UNAVAILABLE_U16,
                                crate::numeric_trace::UNAVAILABLE_U16,
                            ));
                            return Err(error.into());
                        }
                    }
                };

                if let Some(Payload::Extended(extended::Extended::ExtendedControl(control))) = message.payload {
                    if control.message_type()
                        == crate::protocol_layer::message::extended::extended_control::ExtendedControlMessageType::EprKeepAliveAck
                    {
                        numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                            crate::numeric_trace::NumericTraceEventKind::EprKeepAlive,
                            crate::numeric_trace::NumericTraceEprKeepAlivePhase::Acknowledged as u8,
                            message.header.message_id(),
                            crate::numeric_trace::UNAVAILABLE_U8,
                            message.header.0,
                            crate::numeric_trace::UNAVAILABLE_U16,
                        ));
                        self.mode = Mode::Epr;
                        self.epr_keep_alive_deadline_tick = Some(Self::deadline_after(3));
                        State::Ready(*power_source)
                    } else {
                        numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                            crate::numeric_trace::NumericTraceEventKind::EprKeepAlive,
                            crate::numeric_trace::NumericTraceEprKeepAlivePhase::UnexpectedResponse as u8,
                            message.header.message_id(),
                            crate::numeric_trace::UNAVAILABLE_U8,
                            message.header.0,
                            crate::numeric_trace::UNAVAILABLE_U16,
                        ));
                        State::SendNotSupported(*power_source)
                    }
                } else {
                    numeric_trace!(crate::numeric_trace::NumericTraceEvent::new(
                        crate::numeric_trace::NumericTraceEventKind::EprKeepAlive,
                        crate::numeric_trace::NumericTraceEprKeepAlivePhase::UnexpectedResponse as u8,
                        message.header.message_id(),
                        crate::numeric_trace::UNAVAILABLE_U8,
                        message.header.0,
                        crate::numeric_trace::UNAVAILABLE_U16,
                    ));
                    State::SendNotSupported(*power_source)
                }
            }
        };

        self.state = new_state;

        Ok(())
    }
}
