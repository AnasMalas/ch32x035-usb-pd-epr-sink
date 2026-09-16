//! Reusable device-policy integration for the maintained `usbpd` sink stack.
//!
//! Applications provide a small [`SinkRuntime`] adapter for their command
//! source, diagnostics, load-control output, and timer. The protocol state,
//! contract invalidation, EPR discovery budget, and command handling remain in
//! this crate rather than being copied into each firmware `main`.

use core::future::Future;

use usbpd::protocol_layer::message::data::alert::AlertDataObject;
use usbpd::protocol_layer::message::data::epr_mode::DataEnterFailed;
use usbpd::protocol_layer::message::data::request::PowerSource;
use usbpd::protocol_layer::message::data::sink_capabilities::SinkCapabilities;
use usbpd::protocol_layer::message::data::source_capabilities::SourceCapabilities as StackSourceCapabilities;
use usbpd::protocol_layer::message::data::source_info::SourceInfo;
use usbpd::protocol_layer::message::extended::sink_capabilities_extended::{
    SinkCapabilitiesExtended, SINK_MODE_AVS_SUPPORTED, SINK_MODE_PPS_SUPPORTED, SINK_MODE_VBUS_POWERED,
};
use usbpd::protocol_layer::message::extended::{pps_status as stack_pps_status, status as stack_status};
pub use usbpd::sink::device_policy_manager::HardResetReason as HardResetCause;
#[cfg(feature = "initial-capabilities-fallback")]
pub use usbpd::sink::device_policy_manager::InitialCapabilitiesTimeoutAction;
use usbpd::sink::device_policy_manager::{
    DevicePolicyManager, Event, HardResetOrigin, RequestRejection, SinkStartup, SoftResetMode,
    StatusQueryFailure as StackStatusQueryFailure, StatusQueryKind as StackStatusQueryKind,
};

use crate::{
    capabilities_from_stack, request_to_stack, CapabilityListError, Command, ContractState, ContractTracker,
    ContractTransition, ControllerAction, ControllerConfig, ControllerError, Demand, EprEntryFallback, EprEntryPolicy,
    EprExitPolicy, EprState, Milliamps, Milliwatts, PdoValidity, PortMode, PpsStatus, RequestPlan, SinkController,
    SourceAlert, SourceStatus, StackConversionError, StatusQuery, StatusQueryFailure, SupplyKind, UserRequest,
};

/// Static power and identity data advertised by the sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SinkPowerDescriptor {
    pub vendor_id: u16,
    pub product_id: u16,
    /// Current in the mandatory 5 V Sink PDO.
    pub maximum_current: Milliamps,
    pub pps_supported: bool,
    pub avs_supported: bool,
    pub spr_minimum_pdp_watts: u8,
    pub spr_operational_pdp_watts: u8,
    pub spr_maximum_pdp_watts: u8,
    pub epr_minimum_pdp_watts: u8,
    pub epr_operational_pdp_watts: u8,
    pub epr_maximum_pdp_watts: u8,
}

/// Complete reusable sink-policy configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SinkConfig {
    pub controller: ControllerConfig,
    pub descriptor: SinkPowerDescriptor,
    /// Product policy for application-load continuity while a USB-PD Request
    /// changes the confirmed wire contract.
    pub transition_load_policy: TransitionLoadPolicy,
    /// Maximum automatic EPR entry attempts during one physical attachment.
    /// Set to zero to require an explicit `enter-epr` command.
    pub max_auto_epr_attempts: u8,
    /// Receive window for fresh Source Capabilities after a sent or received
    /// Hard Reset. The PHY remains armed throughout this interval.
    pub hard_reset_recovery_ms: u64,
}

/// Application-load behavior while an electrically significant USB-PD
/// contract transition is in progress.
///
/// This policy applies only to transitions classified by the PD library.
/// Physical detector loss, detach, Hard Reset, protocol loss, and terminal
/// faults still use the unconditional PD-inhibit path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionLoadPolicy {
    /// Inhibit the load before the Request and clear the application latch.
    /// A later PS_RDY restores PD permission, but the user must re-arm output.
    InhibitUntilManualRearm,
    /// Inhibit the load before the Request, preserve the application latch,
    /// and restore PD permission after PS_RDY.
    InhibitUntilReady,
    /// Keep the application load uninterrupted. Select this only when the
    /// complete downstream power path is rated for every requested transition.
    Uninterrupted,
}

/// Invalid or internally inconsistent [`SinkConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkConfigError {
    /// The maintained protocol stack always transmits extended messages in
    /// chunked mode and must not advertise unchunked support in an RDO.
    UnchunkedExtendedMessagesUnsupported,
    SinkCurrentOutOfRange(Milliamps),
    SinkCurrentResolution(Milliamps),
    EprOperationalPdpMissing,
    EprOperationalPdpInvalid(Milliwatts),
    EprOperationalPdpMismatch {
        controller: Milliwatts,
        descriptor_watts: u8,
    },
    EprFieldsWithoutEprSupport,
    AutomaticEprWithoutEprSupport,
}

impl SinkConfig {
    /// Validate values that would otherwise be truncated or advertised
    /// inconsistently on the wire.
    pub fn validate(self) -> Result<Self, SinkConfigError> {
        if self.controller.request_context.flags.unchunked_extended_messages_supported {
            return Err(SinkConfigError::UnchunkedExtendedMessagesUnsupported);
        }

        let current = self.descriptor.maximum_current;
        if !(10..=5_000).contains(&current.get()) {
            return Err(SinkConfigError::SinkCurrentOutOfRange(current));
        }
        if !current.get().is_multiple_of(10) {
            return Err(SinkConfigError::SinkCurrentResolution(current));
        }

        let epr_capable = self.controller.request_context.flags.epr_capable;
        let descriptor_has_epr = self.descriptor.epr_minimum_pdp_watts != 0
            || self.descriptor.epr_operational_pdp_watts != 0
            || self.descriptor.epr_maximum_pdp_watts != 0
            || self.descriptor.avs_supported;

        if epr_capable {
            let operational = self.controller.epr_operational_pdp.ok_or(SinkConfigError::EprOperationalPdpMissing)?;
            if operational.get() == 0 || operational.get() > 255_000 || !operational.get().is_multiple_of(1_000) {
                return Err(SinkConfigError::EprOperationalPdpInvalid(operational));
            }
            if operational.get() / 1_000 != u32::from(self.descriptor.epr_operational_pdp_watts) {
                return Err(SinkConfigError::EprOperationalPdpMismatch {
                    controller: operational,
                    descriptor_watts: self.descriptor.epr_operational_pdp_watts,
                });
            }
        } else {
            if descriptor_has_epr {
                return Err(SinkConfigError::EprFieldsWithoutEprSupport);
            }
            if self.max_auto_epr_attempts != 0 {
                return Err(SinkConfigError::AutomaticEprWithoutEprSupport);
            }
        }

        Ok(self)
    }
}

/// Caller-provided intent for recovering a contract after a short local MCU
/// restart while the same physical PD attachment may still be present.
///
/// This value is deliberately not persisted or inferred by the library. A
/// cold boot must use [`SinkDevice::new`]. The caller is responsible for
/// supplying this only from trustworthy, short-lived reset/session evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryIntent {
    /// PD mode that the Source should have retained across the local reset.
    pub mode: PortMode,
    /// Product-level target to re-plan against the Source's fresh capabilities.
    pub request: UserRequest,
    /// Re-enable the application output latch only after the recovered Request
    /// has reached Accept and PS_RDY.
    pub restore_output: bool,
    /// Maximum automatic Request attempts. Only a transient `Wait` is retried;
    /// explicit rejection and session-loss conditions cancel immediately.
    pub maximum_attempts: u8,
}

/// Invalid warm-recovery construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryInitError {
    /// The ordinary sink configuration is invalid.
    SinkConfig(SinkConfigError),
    /// Recovery must permit at least one Request attempt.
    AttemptsZero,
    /// EPR recovery was requested for a sink not configured for EPR.
    EprNotSupported,
}

/// Why an armed warm-recovery operation stopped without restoring the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RecoveryCancellationReason {
    /// Output Off or another out-of-band application cancellation was observed.
    ApplicationRequested = 0,
    /// A newer explicit request replaced the automatic recovery target.
    Superseded = 1,
    /// The Source explicitly rejected the target.
    RequestRejected = 2,
    /// The configured transient retry budget was consumed.
    AttemptsExhausted = 3,
    /// Fresh capabilities could not satisfy the retained target.
    TargetUnavailable = 4,
    /// Fresh capabilities did not match the retained SPR/EPR mode.
    ModeChanged = 5,
    /// A Hard Reset invalidated the presumed surviving contract.
    HardReset = 6,
    /// Physical detach invalidated the port session.
    Detached = 7,
    /// The protocol/PHY session became unusable.
    ProtocolLost = 8,
}

/// Result of previewing one advertised PDO without changing the live contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityPlan {
    Unavailable { position: u8, validity: PdoValidity },
    Ready(RequestPlan),
    Rejected(ControllerError),
}

/// Whether a Hard Reset was observed from the source or initiated locally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardResetDirection {
    Received,
    Sent,
}

/// Source response to the most recent request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestResult {
    Rejected,
    Deferred,
}

/// Typed runtime observation emitted by [`SinkDevice`].
///
/// An application can format these for USB CDC, SDI, a display, or no output
/// at all without coupling protocol behavior to a particular transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkEvent {
    SourceCapabilities(crate::SourceCapabilities),
    CapabilityPlansStarted {
        count: u8,
    },
    CapabilityPlansUnavailable,
    CapabilityPlan(CapabilityPlan),
    ContractTransitionStarted(ContractTransition),
    Requesting(RequestPlan),
    ContractReady(Option<RequestPlan>),
    ContractRefreshStarted(RequestPlan),
    ContractRefreshed(RequestPlan),
    ControllerRejected(ControllerError),
    StackCapabilitiesRejected(CapabilityListError),
    StackRequestRejected(StackConversionError),
    SourceInfo {
        present_watts: u8,
        maximum_watts: u8,
        reported_watts: u8,
    },
    SourceAlert(SourceAlert),
    SourceStatus(SourceStatus),
    PpsStatus(PpsStatus),
    StatusQueryFailed {
        query: StatusQuery,
        failure: StatusQueryFailure,
    },
    RequestResult(RequestResult),
    RecoveryStarted(RecoveryIntent),
    RecoveryAttemptStarted {
        attempt: u8,
        maximum_attempts: u8,
    },
    RecoveryDeferred {
        attempt: u8,
        maximum_attempts: u8,
    },
    RecoverySucceeded {
        attempt: u8,
        plan: RequestPlan,
        output_restored: bool,
    },
    RecoveryCancelled {
        reason: RecoveryCancellationReason,
        attempts: u8,
        maximum_attempts: u8,
    },
    #[cfg(feature = "hard-reset-reasons")]
    HardReset {
        direction: HardResetDirection,
        cause: HardResetCause,
        recovery_ms: u64,
    },
    #[cfg(not(feature = "hard-reset-reasons"))]
    HardReset {
        direction: HardResetDirection,
        recovery_ms: u64,
    },
    HardResetRecoveryComplete,
    Detached,
    ProtocolLost {
        epr_attempts: u8,
        maximum_epr_attempts: u8,
    },
    EprEntryFailed {
        reason: u8,
    },
    EprDiscoveryStarted {
        attempt: u8,
        maximum_attempts: u8,
    },
    EprDiscoveryUnavailable,
    EprAutomaticDiscoveryDisabled,
    EprManualEntryStarted,
    IdentityRequested,
    HelpRequested,
}

/// Application-owned services used by the reusable policy manager.
///
/// `wait_for_command` must be cancellation-safe because the policy engine may
/// temporarily abandon the future to service a source-initiated message.
pub trait SinkRuntime {
    /// Update the PD policy's permission for the application load.
    ///
    /// This is an immediate control input, not global ownership of the load.
    /// An application may deliberately allow its user latch to control a
    /// non-PD or otherwise unmanaged supply. It must still honor `false` while
    /// a PD contract/session is under policy control, and combine its final
    /// output with board-defined VBUS-valid and fault inputs.
    fn set_pd_load_permitted(&mut self, permitted: bool);
    /// Choose product recovery when initial Source_Capabilities remain silent.
    ///
    /// The default preserves the ordinary standards-oriented Hard Reset.
    #[cfg(feature = "initial-capabilities-fallback")]
    fn initial_capabilities_timeout(&mut self) -> InitialCapabilitiesTimeoutAction {
        InitialCapabilitiesTimeoutAction::HardReset
    }
    /// Shorten only the receive window following a sink-sent Hard Reset caused
    /// by initial capability silence. Other Hard Resets retain `configured_ms`.
    #[cfg(feature = "initial-capabilities-fallback")]
    fn initial_capabilities_hard_reset_recovery_millis(&self, configured_ms: u64) -> u64 {
        configured_ms
    }
    /// Observe entry into passive default-power operation. PD permission has
    /// already been restored, but the user output latch remains application-owned.
    #[cfg(feature = "initial-capabilities-fallback")]
    fn on_default_power_ready(&mut self) {}
    /// Apply the configured load-continuity policy before a PD Request.
    ///
    /// The default is suitable for applications whose
    /// [`SinkRuntime::set_pd_load_permitted`] and
    /// [`SinkRuntime::set_user_output_enabled`] methods directly control the
    /// two gates. Applications with a dedicated supervisor can override this
    /// method to deliver one typed, prompt transition command. It must never
    /// wait for telemetry or other slow work.
    fn apply_transition_load_policy(&mut self, policy: TransitionLoadPolicy, transition: ContractTransition) {
        if !transition.inhibits_load() {
            return;
        }
        match policy {
            TransitionLoadPolicy::InhibitUntilManualRearm => {
                self.set_pd_load_permitted(false);
                self.set_user_output_enabled(false);
            }
            TransitionLoadPolicy::InhibitUntilReady => self.set_pd_load_permitted(false),
            TransitionLoadPolicy::Uninterrupted => {}
        }
    }
    /// Update the application-owned user output latch. This must not submit a
    /// PD request, alter desired contract state, or enter/exit EPR.
    fn set_user_output_enabled(&mut self, enabled: bool);
    fn clear_pending_commands(&mut self);
    /// Return true when an out-of-band application path has cancelled warm
    /// recovery, for example an immediate `Output Off` command handled while
    /// the policy engine is waiting for a PD response.
    ///
    /// Implement this when output commands bypass [`SinkDevice::get_event`].
    /// The default is suitable when all cancellation enters through the DPM.
    fn recovery_cancel_requested(&self) -> bool {
        false
    }
    /// Catch-all observation hook. Applications that care about code size can
    /// override the typed `on_*` methods below; their defaults forward here.
    fn observe(&mut self, _event: SinkEvent) {}
    fn on_source_capabilities(&mut self, capabilities: crate::SourceCapabilities) {
        self.observe(SinkEvent::SourceCapabilities(capabilities));
    }
    fn on_capability_plans_started(&mut self, count: u8) {
        self.observe(SinkEvent::CapabilityPlansStarted { count });
    }
    fn on_capability_plans_unavailable(&mut self) {
        self.observe(SinkEvent::CapabilityPlansUnavailable);
    }
    fn on_capability_plan(&mut self, plan: CapabilityPlan) {
        self.observe(SinkEvent::CapabilityPlan(plan));
    }
    /// A Request is about to start. Load inhibition, when required, has
    /// already been issued independently of this observation hook.
    fn on_contract_transition_started(&mut self, transition: ContractTransition) {
        self.observe(SinkEvent::ContractTransitionStarted(transition));
    }
    fn on_requesting(&mut self, plan: RequestPlan) {
        self.observe(SinkEvent::Requesting(plan));
    }
    fn on_contract_ready(&mut self, plan: Option<RequestPlan>) {
        self.observe(SinkEvent::ContractReady(plan));
    }
    /// An identical Request is being sent to maintain an existing contract.
    /// The power path remains enabled because no wire-encoded parameter is
    /// changing.
    fn on_contract_refresh_started(&mut self, plan: RequestPlan) {
        self.observe(SinkEvent::ContractRefreshStarted(plan));
    }
    /// The Source accepted an identical contract-maintenance Request.
    fn on_contract_refreshed(&mut self, plan: RequestPlan) {
        self.observe(SinkEvent::ContractRefreshed(plan));
    }
    fn on_controller_rejected(&mut self, error: ControllerError) {
        self.observe(SinkEvent::ControllerRejected(error));
    }
    fn on_stack_capabilities_rejected(&mut self, error: CapabilityListError) {
        self.observe(SinkEvent::StackCapabilitiesRejected(error));
    }
    fn on_stack_request_rejected(&mut self, error: StackConversionError) {
        self.observe(SinkEvent::StackRequestRejected(error));
    }
    fn on_source_info(&mut self, present_watts: u8, maximum_watts: u8, reported_watts: u8) {
        self.observe(SinkEvent::SourceInfo { present_watts, maximum_watts, reported_watts });
    }
    /// Alert is asynchronous; an operating-condition change can indicate a
    /// PPS CV/CL transition. The reusable DPM follows it with `Get_Status`.
    fn on_source_alert(&mut self, alert: SourceAlert) {
        self.observe(SinkEvent::SourceAlert(alert));
    }
    /// General Source status. `pps_operating_mode()` is suitable for an
    /// application-owned CL indicator LED when it returns `Some`.
    fn on_source_status(&mut self, status: SourceStatus) {
        self.observe(SinkEvent::SourceStatus(status));
    }
    /// Live PPS voltage/current/temperature and CV/CL mode.
    fn on_pps_status(&mut self, status: PpsStatus) {
        self.observe(SinkEvent::PpsStatus(status));
    }
    fn on_status_query_failed(&mut self, query: StatusQuery, failure: StatusQueryFailure) {
        self.observe(SinkEvent::StatusQueryFailed { query, failure });
    }
    fn on_request_result(&mut self, result: RequestResult) {
        self.observe(SinkEvent::RequestResult(result));
    }
    fn on_recovery_started(&mut self, intent: RecoveryIntent) {
        self.observe(SinkEvent::RecoveryStarted(intent));
    }
    fn on_recovery_attempt_started(&mut self, attempt: u8, maximum_attempts: u8) {
        self.observe(SinkEvent::RecoveryAttemptStarted { attempt, maximum_attempts });
    }
    fn on_recovery_deferred(&mut self, attempt: u8, maximum_attempts: u8) {
        self.observe(SinkEvent::RecoveryDeferred { attempt, maximum_attempts });
    }
    fn on_recovery_succeeded(&mut self, attempt: u8, plan: RequestPlan, output_restored: bool) {
        self.observe(SinkEvent::RecoverySucceeded { attempt, plan, output_restored });
    }
    fn on_recovery_cancelled(&mut self, reason: RecoveryCancellationReason, attempts: u8, maximum_attempts: u8) {
        self.observe(SinkEvent::RecoveryCancelled { reason, attempts, maximum_attempts });
    }
    #[cfg(feature = "hard-reset-reasons")]
    fn on_hard_reset(&mut self, direction: HardResetDirection, cause: HardResetCause, recovery_ms: u64) {
        self.observe(SinkEvent::HardReset { direction, cause, recovery_ms });
    }
    #[cfg(not(feature = "hard-reset-reasons"))]
    fn on_hard_reset(&mut self, direction: HardResetDirection, recovery_ms: u64) {
        self.observe(SinkEvent::HardReset { direction, recovery_ms });
    }
    fn on_hard_reset_recovery_complete(&mut self) {
        self.observe(SinkEvent::HardResetRecoveryComplete);
    }
    fn on_detached(&mut self) {
        self.observe(SinkEvent::Detached);
    }
    fn on_protocol_lost(&mut self, epr_attempts: u8, maximum_epr_attempts: u8) {
        self.observe(SinkEvent::ProtocolLost { epr_attempts, maximum_epr_attempts });
    }
    fn on_epr_entry_failed(&mut self, reason: u8) {
        self.observe(SinkEvent::EprEntryFailed { reason });
    }
    fn on_epr_discovery_started(&mut self, attempt: u8, maximum_attempts: u8) {
        self.observe(SinkEvent::EprDiscoveryStarted { attempt, maximum_attempts });
    }
    fn on_epr_discovery_unavailable(&mut self) {
        self.observe(SinkEvent::EprDiscoveryUnavailable);
    }
    fn on_epr_automatic_discovery_disabled(&mut self) {
        self.observe(SinkEvent::EprAutomaticDiscoveryDisabled);
    }
    fn on_epr_manual_entry_started(&mut self) {
        self.observe(SinkEvent::EprManualEntryStarted);
    }
    fn on_identity_requested(&mut self) {
        self.observe(SinkEvent::IdentityRequested);
    }
    fn on_help_requested(&mut self) {
        self.observe(SinkEvent::HelpRequested);
    }
    fn capability_plans_enabled(&self) -> bool {
        true
    }
    fn wait_for_command(&mut self) -> impl Future<Output = Command>;
    fn delay_millis(&mut self, milliseconds: u64) -> impl Future<Output = ()>;
}

/// Reusable Device Policy Manager for the maintained `usbpd` sink engine.
pub struct SinkDevice<R: SinkRuntime> {
    config: SinkConfig,
    runtime: R,
    controller: SinkController,
    contract: ContractTracker,
    pending_contract_refresh: bool,
    source_info_requested: bool,
    source_status_pending: bool,
    epr_discovery_attempts: u8,
    epr_exhaustion_reported: bool,
    recovery: Option<RecoveryState>,
}

#[derive(Clone, Copy, Debug)]
struct RecoveryState {
    intent: RecoveryIntent,
    attempts: u8,
    request_pending: bool,
}

impl<R: SinkRuntime> SinkDevice<R> {
    pub fn new(config: SinkConfig, runtime: R) -> Result<Self, SinkConfigError> {
        let config = config.validate()?;
        Ok(Self::from_validated(config, runtime, None))
    }

    /// Create a DPM that starts by Soft Resetting a presumed live PD session
    /// and requesting the caller's retained target.
    ///
    /// The load is inhibited immediately. No output restoration occurs until
    /// the new Request reaches Accept and PS_RDY.
    pub fn new_recovering(
        config: SinkConfig,
        mut runtime: R,
        intent: RecoveryIntent,
    ) -> Result<Self, RecoveryInitError> {
        let config = config.validate().map_err(RecoveryInitError::SinkConfig)?;
        if intent.maximum_attempts == 0 {
            return Err(RecoveryInitError::AttemptsZero);
        }
        if matches!(intent.mode, PortMode::Epr) && !config.controller.request_context.flags.epr_capable {
            return Err(RecoveryInitError::EprNotSupported);
        }

        runtime.set_pd_load_permitted(false);
        runtime.set_user_output_enabled(false);
        runtime.on_recovery_started(intent);
        Ok(Self::from_validated(config, runtime, Some(RecoveryState { intent, attempts: 0, request_pending: false })))
    }

    fn from_validated(config: SinkConfig, runtime: R, recovery: Option<RecoveryState>) -> Self {
        Self {
            controller: SinkController::new(config.controller),
            config,
            runtime,
            contract: ContractTracker::new(),
            pending_contract_refresh: false,
            source_info_requested: false,
            source_status_pending: false,
            epr_discovery_attempts: 0,
            epr_exhaustion_reported: false,
            recovery,
        }
    }

    pub const fn config(&self) -> &SinkConfig {
        &self.config
    }

    pub const fn controller(&self) -> &SinkController {
        &self.controller
    }

    pub const fn contract(&self) -> &ContractTracker {
        &self.contract
    }

    pub fn runtime_mut(&mut self) -> &mut R {
        &mut self.runtime
    }

    pub const fn recovery_intent(&self) -> Option<RecoveryIntent> {
        match self.recovery {
            Some(recovery) => Some(recovery.intent),
            None => None,
        }
    }

    pub const fn recovery_attempts(&self) -> u8 {
        match self.recovery {
            Some(recovery) => recovery.attempts,
            None => 0,
        }
    }

    fn cancel_recovery(&mut self, reason: RecoveryCancellationReason, clear_desired: bool) {
        let Some(recovery) = self.recovery.take() else {
            return;
        };
        if clear_desired {
            self.controller.clear_desired();
        }
        self.runtime.on_recovery_cancelled(reason, recovery.attempts, recovery.intent.maximum_attempts);
    }

    fn recovery_plan(&mut self) -> Option<RequestPlan> {
        let recovery = self.recovery?;
        if self.runtime.recovery_cancel_requested() {
            self.cancel_recovery(RecoveryCancellationReason::ApplicationRequested, true);
            return None;
        }

        let mode_matches = matches!(
            (recovery.intent.mode, self.controller.epr_state()),
            (PortMode::Spr, EprState::Spr) | (PortMode::Epr, EprState::Epr)
        );
        if !mode_matches {
            self.cancel_recovery(RecoveryCancellationReason::ModeChanged, true);
            return None;
        }
        if recovery.attempts >= recovery.intent.maximum_attempts {
            self.cancel_recovery(RecoveryCancellationReason::AttemptsExhausted, true);
            return None;
        }

        let plan = match self.controller.preview(recovery.intent.request) {
            Ok(plan) => plan,
            Err(error) => {
                self.runtime.on_controller_rejected(error);
                self.cancel_recovery(RecoveryCancellationReason::TargetUnavailable, true);
                return None;
            }
        };
        self.controller.retain_desired(recovery.intent.request);
        let recovery = self.recovery.as_mut().expect("recovery remains armed while planning");
        recovery.attempts += 1;
        recovery.request_pending = true;
        self.runtime.on_recovery_attempt_started(recovery.attempts, recovery.intent.maximum_attempts);
        Some(plan)
    }

    fn begin_request(&mut self, plan: RequestPlan) -> bool {
        let transition = self.contract.classify_transition(plan);
        self.runtime.apply_transition_load_policy(self.config.transition_load_policy, transition);
        self.runtime.on_contract_transition_started(transition);
        self.contract.on_request(plan).expect("request must follow advertised capabilities");
        self.pending_contract_refresh = transition.is_identical_refresh();
        self.pending_contract_refresh
    }

    fn report_contract(&mut self) {
        self.runtime.on_contract_ready(self.contract.active_plan());
    }

    fn invalidate_session(&mut self, reason: RecoveryCancellationReason) {
        self.runtime.set_pd_load_permitted(false);
        self.runtime.clear_pending_commands();
        if matches!(reason, RecoveryCancellationReason::Detached) {
            self.contract.on_detach();
        } else {
            self.contract.on_protocol_loss();
        }
        self.pending_contract_refresh = false;
        self.cancel_recovery(reason, false);
        self.controller.reset_port();
        self.source_info_requested = false;
        self.source_status_pending = false;
    }

    fn event_for_action(&mut self, action: ControllerAction) -> Event {
        match action {
            ControllerAction::Request(plan) => match request_to_stack(plan) {
                Ok(request) => {
                    if self.begin_request(plan) {
                        self.runtime.on_contract_refresh_started(plan);
                    } else {
                        self.runtime.on_requesting(plan);
                    }
                    Event::RequestPower(request)
                }
                Err(error) => {
                    self.runtime.on_stack_request_rejected(error);
                    Event::None
                }
            },
            ControllerAction::EnterEprMode { operational_pdp } => {
                Event::enter_epr_mode_watts((operational_pdp.get() / 1_000) as u8)
            }
            ControllerAction::RequestEprCapabilities => Event::RequestEprSourceCapabilities,
            ControllerAction::ExitEprMode => Event::ExitEprMode,
            ControllerAction::None => Event::None,
        }
    }

    fn capabilities_or_report(
        &mut self,
        source_capabilities: &StackSourceCapabilities,
    ) -> Option<crate::SourceCapabilities> {
        match capabilities_from_stack(source_capabilities) {
            Ok(capabilities) => Some(capabilities),
            Err(error) => {
                self.runtime.on_stack_capabilities_rejected(error);
                None
            }
        }
    }

    fn automatic_epr_enabled(&self) -> bool {
        self.config.max_auto_epr_attempts != 0 && self.config.controller.request_context.flags.epr_capable
    }

    #[cfg(all(feature = "initial-capabilities-fallback", feature = "hard-reset-reasons"))]
    fn hard_reset_recovery_ms_for(&self, origin: HardResetOrigin, reason: HardResetCause) -> u64 {
        if origin == HardResetOrigin::Sink && reason == HardResetCause::SourceCapabilitiesTimeout {
            self.runtime.initial_capabilities_hard_reset_recovery_millis(self.config.hard_reset_recovery_ms)
        } else {
            self.config.hard_reset_recovery_ms
        }
    }
}

impl<R: SinkRuntime> DevicePolicyManager for SinkDevice<R> {
    fn startup(&self) -> SinkStartup {
        match self.recovery.map(|recovery| recovery.intent.mode) {
            None => SinkStartup::Fresh,
            Some(PortMode::Spr) => SinkStartup::SoftReset(SoftResetMode::Spr),
            Some(PortMode::Epr) => SinkStartup::SoftReset(SoftResetMode::Epr),
        }
    }

    #[cfg(feature = "initial-capabilities-fallback")]
    fn initial_capabilities_timeout(&mut self) -> InitialCapabilitiesTimeoutAction {
        self.runtime.initial_capabilities_timeout()
    }

    #[cfg(feature = "initial-capabilities-fallback")]
    fn default_power_ready(&mut self) {
        if matches!(self.contract.state(), ContractState::Detached | ContractState::Lost) {
            self.contract.on_attach();
        }
        self.runtime.set_pd_load_permitted(true);
        self.runtime.on_default_power_ready();
    }

    fn sink_capabilities(&self) -> SinkCapabilities {
        SinkCapabilities::new_vsafe5v_only((self.config.descriptor.maximum_current.get() / 10) as u16)
    }

    fn sink_capabilities_extended(&self) -> SinkCapabilitiesExtended {
        let descriptor = self.config.descriptor;
        let mut sink_modes = SINK_MODE_VBUS_POWERED;
        if descriptor.pps_supported {
            sink_modes |= SINK_MODE_PPS_SUPPORTED;
        }
        if descriptor.avs_supported {
            sink_modes |= SINK_MODE_AVS_SUPPORTED;
        }

        SinkCapabilitiesExtended::new_v1_power_descriptor(
            descriptor.vendor_id,
            descriptor.product_id,
            sink_modes,
            descriptor.spr_minimum_pdp_watts,
            descriptor.spr_operational_pdp_watts,
            descriptor.spr_maximum_pdp_watts,
            descriptor.epr_minimum_pdp_watts,
            descriptor.epr_operational_pdp_watts,
            descriptor.epr_maximum_pdp_watts,
        )
    }

    fn inform(&mut self, source_capabilities: &StackSourceCapabilities) {
        if matches!(self.contract.state(), ContractState::Detached | ContractState::Lost) {
            self.contract.on_attach();
        }
        self.contract.on_capabilities().expect("capabilities require an attached port");

        if let Some(capabilities) = self.capabilities_or_report(source_capabilities) {
            self.runtime.on_source_capabilities(capabilities);
            self.controller.observe_capabilities(capabilities);
            self.source_info_requested = false;
        }
    }

    fn request(&mut self, _source_capabilities: &StackSourceCapabilities) -> PowerSource {
        let plan = match self.recovery_plan() {
            Some(plan) => plan,
            None => self
                .controller
                .request_for_current_capabilities_with_contract(self.contract.active_plan())
                .expect("every accepted source must advertise a valid fixed 5 V PDO"),
        };
        if let Some(error) = self.controller.take_pending_error() {
            self.runtime.on_controller_rejected(error);
        }

        if self.begin_request(plan) {
            self.runtime.on_contract_refresh_started(plan);
        } else {
            self.runtime.on_requesting(plan);
        }
        request_to_stack(plan).expect("request planner must return a stack-representable plan")
    }

    fn transition_power(&mut self, _accepted: &PowerSource) {
        self.contract.on_accept().expect("PS_RDY must correspond to a pending request");
        self.contract.on_ps_ready().expect("accepted request must become the active contract");
        self.controller.on_ps_ready();
        let recovery_cancel_requested =
            self.recovery.is_some_and(|recovery| recovery.request_pending && self.runtime.recovery_cancel_requested());
        if recovery_cancel_requested {
            self.runtime.set_user_output_enabled(false);
        }
        self.runtime.set_pd_load_permitted(true);
        if self.recovery.is_some_and(|recovery| recovery.request_pending) {
            if recovery_cancel_requested {
                self.cancel_recovery(RecoveryCancellationReason::ApplicationRequested, false);
            } else {
                let recovery = self.recovery.take().expect("pending recovery must remain armed");
                if recovery.intent.restore_output {
                    self.runtime.set_user_output_enabled(true);
                }
                self.runtime.on_recovery_succeeded(
                    recovery.attempts,
                    self.contract.active_plan().expect("recovery PS_RDY must confirm a contract"),
                    recovery.intent.restore_output,
                );
            }
        }
        if self.pending_contract_refresh {
            self.pending_contract_refresh = false;
            self.runtime.on_contract_refreshed(
                self.contract.active_plan().expect("a completed refresh must retain a contract"),
            );
        } else {
            self.report_contract();
        }
    }

    fn request_not_accepted(&mut self, reason: RequestRejection) {
        self.contract.on_reject_or_wait();
        self.pending_contract_refresh = false;
        let result = match reason {
            RequestRejection::Reject => {
                self.controller.request_rejected();
                RequestResult::Rejected
            }
            RequestRejection::Wait => {
                self.controller.request_deferred();
                RequestResult::Deferred
            }
        };
        if self.recovery.is_some_and(|recovery| recovery.request_pending) {
            let recovery = self.recovery.as_mut().expect("checked above");
            recovery.request_pending = false;
            if self.runtime.recovery_cancel_requested() {
                self.cancel_recovery(RecoveryCancellationReason::ApplicationRequested, true);
            } else {
                match reason {
                    RequestRejection::Reject => {
                        self.cancel_recovery(RecoveryCancellationReason::RequestRejected, true);
                    }
                    RequestRejection::Wait => {
                        let recovery = self.recovery.expect("Wait keeps recovery armed");
                        if recovery.attempts >= recovery.intent.maximum_attempts {
                            self.cancel_recovery(RecoveryCancellationReason::AttemptsExhausted, true);
                        } else {
                            self.runtime.on_recovery_deferred(recovery.attempts, recovery.intent.maximum_attempts);
                        }
                    }
                }
            }
        }
        if self.contract.load_may_enable() {
            self.runtime.set_pd_load_permitted(true);
        }
        self.runtime.on_request_result(result);
    }

    fn inform_source_info(&mut self, source_info: &SourceInfo) {
        let present_watts = source_info.port_present_pdp_watts();
        let present_pdp = (present_watts != 0).then_some(Milliwatts(u32::from(present_watts) * 1_000));
        self.controller.set_source_present_pdp(present_pdp);
        self.runtime.on_source_info(
            present_watts,
            source_info.object1.port_maximum_pdp_watts(),
            source_info.object1.port_reported_pdp_watts(),
        );
    }

    fn inform_alert(&mut self, alert: &AlertDataObject) {
        self.runtime.on_source_alert(SourceAlert::from_raw(alert.0));
        if alert.has_non_battery_status_change() {
            self.source_status_pending = true;
        }
    }

    fn inform_status(&mut self, status: &stack_status::Status) {
        let pps_mode_valid = matches!(self.contract.active_plan().map(RequestPlan::supply), Some(SupplyKind::Pps));
        self.runtime.on_source_status(SourceStatus::from_raw_bytes(status.raw_bytes(), pps_mode_valid));
    }

    fn inform_pps_status(&mut self, status: &stack_pps_status::PpsStatus) {
        self.runtime.on_pps_status(PpsStatus::from_raw_bytes(status.raw_bytes()));
    }

    fn status_query_failed(&mut self, query: StackStatusQueryKind, failure: StackStatusQueryFailure) {
        let query = match query {
            StackStatusQueryKind::General => StatusQuery::General,
            StackStatusQueryKind::Pps => StatusQuery::Pps,
        };
        let failure = match failure {
            StackStatusQueryFailure::NotSupported => StatusQueryFailure::NotSupported,
            StackStatusQueryFailure::Rejected => StatusQueryFailure::Rejected,
            StackStatusQueryFailure::Deferred => StatusQueryFailure::Deferred,
            StackStatusQueryFailure::Timeout => StatusQueryFailure::Timeout,
        };
        self.runtime.on_status_query_failed(query, failure);
    }

    #[cfg(feature = "hard-reset-reasons")]
    fn hard_reset(&mut self, origin: HardResetOrigin, reason: HardResetCause) {
        self.invalidate_session(RecoveryCancellationReason::HardReset);
        let direction = match origin {
            HardResetOrigin::Source => HardResetDirection::Received,
            HardResetOrigin::Sink => HardResetDirection::Sent,
        };
        #[cfg(feature = "initial-capabilities-fallback")]
        let recovery_ms = self.hard_reset_recovery_ms_for(origin, reason);
        #[cfg(not(feature = "initial-capabilities-fallback"))]
        let recovery_ms = self.config.hard_reset_recovery_ms;
        self.runtime.on_hard_reset(direction, reason, recovery_ms);
    }

    #[cfg(not(feature = "hard-reset-reasons"))]
    fn hard_reset(&mut self, origin: HardResetOrigin) {
        self.invalidate_session(RecoveryCancellationReason::HardReset);
        let direction = match origin {
            HardResetOrigin::Source => HardResetDirection::Received,
            HardResetOrigin::Sink => HardResetDirection::Sent,
        };
        self.runtime.on_hard_reset(direction, self.config.hard_reset_recovery_ms);
    }

    fn hard_reset_recovery_millis(&self) -> u32 {
        self.config.hard_reset_recovery_ms.min(u64::from(u32::MAX)) as u32
    }

    #[cfg(all(feature = "initial-capabilities-fallback", feature = "hard-reset-reasons"))]
    fn hard_reset_recovery_millis_for(&self, origin: HardResetOrigin, reason: HardResetCause) -> u32 {
        self.hard_reset_recovery_ms_for(origin, reason).min(u64::from(u32::MAX)) as u32
    }

    fn hard_reset_recovered(&mut self) {
        self.runtime.on_hard_reset_recovery_complete();
    }

    fn detached(&mut self) {
        self.invalidate_session(RecoveryCancellationReason::Detached);
        self.epr_discovery_attempts = 0;
        self.epr_exhaustion_reported = false;
        self.runtime.on_detached();
    }

    fn protocol_lost(&mut self) {
        self.invalidate_session(RecoveryCancellationReason::ProtocolLost);
        self.runtime.on_protocol_lost(self.epr_discovery_attempts, self.config.max_auto_epr_attempts);
    }

    fn epr_mode_entry_failed(&mut self, reason: DataEnterFailed) {
        self.controller.epr_entry_failed();
        self.pending_contract_refresh = false;
        self.source_info_requested = false;
        self.source_status_pending = false;
        self.epr_discovery_attempts = self.config.max_auto_epr_attempts;
        self.runtime.on_epr_entry_failed(u8::from(reason));
    }

    async fn get_event(&mut self, source_capabilities: &StackSourceCapabilities) -> Event {
        loop {
            if let Some(action) = self.controller.take_ready_action() {
                return self.event_for_action(action);
            }

            if self.source_status_pending {
                self.source_status_pending = false;
                return Event::RequestStatus;
            }

            if self.contract.load_may_enable() && !self.source_info_requested {
                self.source_info_requested = true;
                return Event::RequestSourceInfo;
            }

            if self.automatic_epr_enabled()
                && self.contract.load_may_enable()
                && self.source_info_requested
                && matches!(self.controller.epr_state(), EprState::Spr)
                && source_capabilities.epr_mode_capable()
                && self.epr_discovery_attempts < self.config.max_auto_epr_attempts
            {
                match self.controller.begin_epr_discovery(
                    EprEntryPolicy::PreserveVoltage { fallback: EprEntryFallback::Safe5V },
                    self.contract.active_plan(),
                ) {
                    Ok(action) => {
                        self.epr_discovery_attempts += 1;
                        self.epr_exhaustion_reported = false;
                        self.runtime
                            .on_epr_discovery_started(self.epr_discovery_attempts, self.config.max_auto_epr_attempts);
                        return self.event_for_action(action);
                    }
                    Err(ControllerError::EprUnavailable | ControllerError::NoCapabilities(_)) => {}
                    Err(_) => {
                        self.epr_discovery_attempts = self.config.max_auto_epr_attempts;
                        self.runtime.on_epr_discovery_unavailable();
                    }
                }
            }

            if self.automatic_epr_enabled()
                && self.contract.load_may_enable()
                && self.source_info_requested
                && matches!(self.controller.epr_state(), EprState::Spr)
                && self.epr_discovery_attempts >= self.config.max_auto_epr_attempts
                && !self.epr_exhaustion_reported
            {
                self.epr_exhaustion_reported = true;
                self.runtime.on_epr_automatic_discovery_disabled();
            }

            let command = self.runtime.wait_for_command().await;
            let action = match command {
                Command::Request(request) => {
                    self.cancel_recovery(RecoveryCancellationReason::Superseded, true);
                    self.controller.submit(request)
                }
                Command::Identity => {
                    self.runtime.on_identity_requested();
                    continue;
                }
                Command::Capabilities => {
                    if let Some(capabilities) = self.capabilities_or_report(source_capabilities) {
                        self.runtime.on_source_capabilities(capabilities);
                    }
                    continue;
                }
                Command::Plans => {
                    if !self.runtime.capability_plans_enabled() {
                        self.runtime.on_capability_plans_unavailable();
                        continue;
                    }
                    if let Some(capabilities) = self.capabilities_or_report(source_capabilities) {
                        self.runtime.on_capability_plans_started(capabilities.len() as u8);
                        for pdo in capabilities.iter() {
                            let result = if pdo.is_requestable() {
                                match self
                                    .controller
                                    .preview(UserRequest::Pdo { position: pdo.position, demand: Demand::Maximum })
                                {
                                    Ok(plan) => CapabilityPlan::Ready(plan),
                                    Err(error) => CapabilityPlan::Rejected(error),
                                }
                            } else {
                                CapabilityPlan::Unavailable { position: pdo.position, validity: pdo.validity }
                            };
                            self.runtime.on_capability_plan(result);
                        }
                        self.report_contract();
                    }
                    continue;
                }
                Command::RequestSourceInfo => {
                    self.source_info_requested = true;
                    return Event::RequestSourceInfo;
                }
                Command::RequestSourceStatus => return Event::RequestStatus,
                Command::RequestPpsStatus => return Event::RequestPpsStatus,
                Command::OutputOn => {
                    self.runtime.set_user_output_enabled(true);
                    continue;
                }
                Command::OutputOff => {
                    self.runtime.set_user_output_enabled(false);
                    self.cancel_recovery(RecoveryCancellationReason::ApplicationRequested, true);
                    continue;
                }
                Command::EnterEpr => {
                    let action = self.controller.begin_epr_discovery(
                        EprEntryPolicy::PreserveVoltage { fallback: EprEntryFallback::Safe5V },
                        self.contract.active_plan(),
                    );
                    if action.is_ok() {
                        self.runtime.on_epr_manual_entry_started();
                    }
                    action
                }
                Command::RequestEprCapabilities => self.controller.request_epr_capabilities(),
                Command::ExitEpr => self.controller.exit_epr(EprExitPolicy::Safe5V, self.contract.active_plan()),
                Command::Status => {
                    self.report_contract();
                    continue;
                }
                Command::Help => {
                    self.runtime.on_help_requested();
                    continue;
                }
            };

            match action {
                Ok(action) => return self.event_for_action(action),
                Err(error) => self.runtime.on_controller_rejected(error),
            }
        }
    }
}
