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
use usbpd::sink::device_policy_manager::{
    DevicePolicyManager, Event, HardResetOrigin, RequestRejection, StatusQueryFailure as StackStatusQueryFailure,
    StatusQueryKind as StackStatusQueryKind,
};

use crate::{
    capabilities_from_stack, request_to_stack, CapabilityListError, Command, ContractState, ContractTracker,
    ControllerAction, ControllerConfig, ControllerError, Demand, EprState, Milliamps, Milliwatts, PdoValidity,
    PpsStatus, RequestPlan, SinkController, SourceAlert, SourceStatus, StackConversionError, StatusQuery,
    StatusQueryFailure, SupplyKind, UserRequest,
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
    /// Maximum automatic EPR entry attempts during one physical attachment.
    /// Set to zero to require an explicit `enter-epr` command.
    pub max_auto_epr_attempts: u8,
    /// Conservative source recovery delay after a sent or received Hard Reset.
    pub hard_reset_recovery_ms: u64,
}

/// Invalid or internally inconsistent [`SinkConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkConfigError {
    SinkCurrentOutOfRange(Milliamps),
    SinkCurrentResolution(Milliamps),
    EprOperationalPdpMissing,
    EprOperationalPdpInvalid(Milliwatts),
    EprOperationalPdpMismatch { controller: Milliwatts, descriptor_watts: u8 },
    EprFieldsWithoutEprSupport,
    AutomaticEprWithoutEprSupport,
}

impl SinkConfig {
    /// Validate values that would otherwise be truncated or advertised
    /// inconsistently on the wire.
    pub fn validate(self) -> Result<Self, SinkConfigError> {
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
    CapabilityPlansStarted { count: u8 },
    CapabilityPlansUnavailable,
    CapabilityPlan(CapabilityPlan),
    Requesting(RequestPlan),
    ContractReady(Option<RequestPlan>),
    ContractRefreshStarted(RequestPlan),
    ContractRefreshed(RequestPlan),
    ControllerRejected(ControllerError),
    StackCapabilitiesRejected(CapabilityListError),
    StackRequestRejected(StackConversionError),
    SourceInfo { present_watts: u8, maximum_watts: u8, reported_watts: u8 },
    SourceAlert(SourceAlert),
    SourceStatus(SourceStatus),
    PpsStatus(PpsStatus),
    StatusQueryFailed { query: StatusQuery, failure: StatusQueryFailure },
    RequestResult(RequestResult),
    HardReset { direction: HardResetDirection, recovery_ms: u64 },
    HardResetRecoveryComplete,
    Detached,
    ProtocolLost { epr_attempts: u8, maximum_epr_attempts: u8 },
    EprEntryFailed { reason: u8 },
    EprDiscoveryStarted { attempt: u8, maximum_attempts: u8 },
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
    fn set_load_enabled(&mut self, enabled: bool);
    fn clear_pending_commands(&mut self);
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
}

impl<R: SinkRuntime> SinkDevice<R> {
    pub fn new(config: SinkConfig, runtime: R) -> Result<Self, SinkConfigError> {
        let config = config.validate()?;
        Ok(Self {
            controller: SinkController::new(config.controller),
            config,
            runtime,
            contract: ContractTracker::new(),
            pending_contract_refresh: false,
            source_info_requested: false,
            source_status_pending: false,
            epr_discovery_attempts: 0,
            epr_exhaustion_reported: false,
        })
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

    fn begin_request(&mut self, plan: RequestPlan) -> bool {
        let is_refresh = !self.contract.request_changes_power(plan);
        if !is_refresh {
            self.runtime.set_load_enabled(false);
        }
        self.contract.on_request(plan).expect("request must follow advertised capabilities");
        self.pending_contract_refresh = is_refresh;
        is_refresh
    }

    fn report_contract(&mut self) {
        self.runtime.on_contract_ready(self.contract.active_plan());
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
}

impl<R: SinkRuntime> DevicePolicyManager for SinkDevice<R> {
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

    async fn inform(&mut self, source_capabilities: &StackSourceCapabilities) {
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

    async fn request(&mut self, source_capabilities: &StackSourceCapabilities) -> PowerSource {
        let capabilities =
            self.capabilities_or_report(source_capabilities).expect("policy engine must bound Source PDO count");
        let plan = self
            .controller
            .request_for_capabilities(capabilities)
            .expect("every accepted source must advertise a valid fixed 5 V PDO");

        if self.begin_request(plan) {
            self.runtime.on_contract_refresh_started(plan);
        } else {
            self.runtime.on_requesting(plan);
        }
        request_to_stack(plan).expect("request planner must return a stack-representable plan")
    }

    async fn transition_power(&mut self, _accepted: &PowerSource) {
        self.contract.on_accept().expect("PS_RDY must correspond to a pending request");
        self.contract.on_ps_ready().expect("accepted request must become the active contract");
        self.controller.on_ps_ready();
        self.runtime.set_load_enabled(true);
        if self.pending_contract_refresh {
            self.pending_contract_refresh = false;
            self.runtime.on_contract_refreshed(
                self.contract.active_plan().expect("a completed refresh must retain a contract"),
            );
        } else {
            self.report_contract();
        }
    }

    async fn request_not_accepted(&mut self, reason: RequestRejection) {
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
        if self.contract.load_may_enable() {
            self.runtime.set_load_enabled(true);
        }
        self.runtime.on_request_result(result);
    }

    async fn inform_source_info(&mut self, source_info: &SourceInfo) {
        let present_watts = source_info.port_present_pdp_watts();
        let present_pdp = (present_watts != 0).then_some(Milliwatts(u32::from(present_watts) * 1_000));
        self.controller.set_source_present_pdp(present_pdp);
        self.runtime.on_source_info(
            present_watts,
            source_info.object1.port_maximum_pdp_watts(),
            source_info.object1.port_reported_pdp_watts(),
        );
    }

    async fn inform_alert(&mut self, alert: &AlertDataObject) {
        self.runtime.on_source_alert(SourceAlert::from_raw(alert.0));
        if alert.has_non_battery_status_change() {
            self.source_status_pending = true;
        }
    }

    async fn inform_status(&mut self, status: &stack_status::Status) {
        let pps_mode_valid = matches!(self.contract.active_plan().map(|plan| plan.supply), Some(SupplyKind::Pps));
        self.runtime.on_source_status(SourceStatus::from_raw_bytes(status.raw_bytes(), pps_mode_valid));
    }

    async fn inform_pps_status(&mut self, status: &stack_pps_status::PpsStatus) {
        self.runtime.on_pps_status(PpsStatus::from_raw_bytes(status.raw_bytes()));
    }

    async fn status_query_failed(&mut self, query: StackStatusQueryKind, failure: StackStatusQueryFailure) {
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

    async fn hard_reset(&mut self, origin: HardResetOrigin) {
        self.runtime.set_load_enabled(false);
        self.runtime.clear_pending_commands();
        self.contract.on_protocol_loss();
        self.pending_contract_refresh = false;
        self.controller.reset_port();
        self.source_info_requested = false;
        self.source_status_pending = false;
        let direction = match origin {
            HardResetOrigin::Source => HardResetDirection::Received,
            HardResetOrigin::Sink => HardResetDirection::Sent,
        };
        self.runtime.on_hard_reset(direction, self.config.hard_reset_recovery_ms);
        self.runtime.delay_millis(self.config.hard_reset_recovery_ms).await;
        self.runtime.on_hard_reset_recovery_complete();
    }

    async fn detached(&mut self) {
        self.runtime.set_load_enabled(false);
        self.runtime.clear_pending_commands();
        self.contract.on_detach();
        self.pending_contract_refresh = false;
        self.controller.reset_port();
        self.source_info_requested = false;
        self.source_status_pending = false;
        self.epr_discovery_attempts = 0;
        self.epr_exhaustion_reported = false;
        self.runtime.on_detached();
    }

    async fn protocol_lost(&mut self) {
        self.runtime.set_load_enabled(false);
        self.runtime.clear_pending_commands();
        self.contract.on_protocol_loss();
        self.pending_contract_refresh = false;
        self.controller.reset_port();
        self.source_info_requested = false;
        self.source_status_pending = false;
        self.runtime.on_protocol_lost(self.epr_discovery_attempts, self.config.max_auto_epr_attempts);
    }

    async fn epr_mode_entry_failed(&mut self, reason: DataEnterFailed) {
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
                match self.controller.begin_epr_discovery() {
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
                Command::Request(request) => self.controller.submit(request),
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
                Command::EnterEpr => {
                    let action = self.controller.begin_epr_discovery();
                    if action.is_ok() {
                        self.runtime.on_epr_manual_entry_started();
                    }
                    action
                }
                Command::RequestEprCapabilities => self.controller.request_epr_capabilities(),
                Command::ExitEpr => self.controller.exit_epr(),
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
