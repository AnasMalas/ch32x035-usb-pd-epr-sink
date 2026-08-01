use crate::capabilities::{CapabilitiesKind, SourceCapabilities, SupplyKind};
use crate::contract::{ContractTransition, ContractTransitionKind};
use crate::request::{
    Demand, PlanError, PortMode, Preference, RequestContext, RequestMessage, RequestPlan, RequestPlanner,
};
use crate::units::{Milliamps, Millivolts, Milliwatts};

/// A user-visible request. Keeping this independent of any command transport
/// lets USB CDC, SDI test commands, and unit tests drive the same policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserRequest {
    Voltage { voltage: Millivolts, current: Option<Milliamps>, preference: Preference },
    Pdo { position: u8, demand: Demand },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ControllerConfig {
    pub request_context: RequestContext,
    /// EPR Sink Operational PDP. It must be explicitly configured as a whole
    /// number of watts before the controller is allowed to enter EPR mode.
    pub epr_operational_pdp: Option<Milliwatts>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EprState {
    #[default]
    Spr,
    Entering,
    Epr,
    Exiting,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EprExitFallback {
    Safe5V,
    #[default]
    Refuse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EprExitPolicy {
    PreserveVoltage { fallback: EprExitFallback },
    Safe5V,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EprExitRefusal {
    NoConfirmedContract = 0,
    NoSuitableSprContract = 1,
    CapabilitiesChanged = 2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EprEntryFallback {
    Safe5V,
    #[default]
    Refuse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EprEntryPolicy {
    PreserveVoltage { fallback: EprEntryFallback },
    Safe5V,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EprEntryRefusal {
    NoConfirmedContract = 0,
    NoSuitableSprContract = 1,
    CapabilitiesChanged = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerAction {
    Request(RequestPlan),
    EnterEprMode { operational_pdp: Milliwatts },
    RequestEprCapabilities,
    ExitEprMode,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerError {
    NoCapabilities(CapabilitiesKind),
    Busy(EprState),
    EprUnavailable,
    EprNotConfigured,
    InvalidEprOperationalPdp(Milliwatts),
    NotInEprMode,
    EprExitRefused(EprExitRefusal),
    EprEntryRefused(EprEntryRefusal),
    Plan(PlanError),
}

impl From<PlanError> for ControllerError {
    fn from(value: PlanError) -> Self {
        Self::Plan(value)
    }
}

/// Product-level sink policy and retained capability model.
///
/// The desired request is deliberately cleared by `reset_port`: reconnecting
/// always starts from a fresh 5 V contract and requires a fresh user command.
#[derive(Clone, Copy, Debug)]
pub struct SinkController {
    planner: RequestPlanner,
    config: ControllerConfig,
    spr_capabilities: Option<SourceCapabilities>,
    epr_capabilities: Option<SourceCapabilities>,
    epr_state: EprState,
    epr_exit_ready: bool,
    epr_exit_fallback: EprExitFallback,
    epr_exit_preserves_voltage: bool,
    epr_entry_pending: bool,
    epr_entry_fallback: EprEntryFallback,
    epr_entry_preserves_voltage: bool,
    pending_error: Option<ControllerError>,
    desired: Option<UserRequest>,
}

impl SinkController {
    pub const fn new(config: ControllerConfig) -> Self {
        Self {
            planner: RequestPlanner::new(),
            config,
            spr_capabilities: None,
            epr_capabilities: None,
            epr_state: EprState::Spr,
            epr_exit_ready: false,
            epr_exit_fallback: EprExitFallback::Refuse,
            epr_exit_preserves_voltage: false,
            epr_entry_pending: false,
            epr_entry_fallback: EprEntryFallback::Refuse,
            epr_entry_preserves_voltage: false,
            pending_error: None,
            desired: None,
        }
    }

    pub const fn epr_state(&self) -> EprState {
        self.epr_state
    }

    pub const fn desired(&self) -> Option<UserRequest> {
        self.desired
    }

    pub fn take_pending_error(&mut self) -> Option<ControllerError> {
        self.pending_error.take()
    }

    /// Update the Source's presently available power. This refines the usable
    /// current reported for power-limited PPS APDOs.
    pub fn set_source_present_pdp(&mut self, pdp: Option<Milliwatts>) {
        self.config.request_context.source_present_pdp = pdp;
    }

    pub const fn capabilities(&self, kind: CapabilitiesKind) -> Option<&SourceCapabilities> {
        match kind {
            CapabilitiesKind::Spr => self.spr_capabilities.as_ref(),
            CapabilitiesKind::Epr => self.epr_capabilities.as_ref(),
        }
    }

    pub fn observe_capabilities(&mut self, capabilities: SourceCapabilities) {
        match capabilities.kind() {
            CapabilitiesKind::Spr => {
                self.spr_capabilities = Some(capabilities);
                // If EPR entry was interrupted before EnterSucceeded (for
                // example by a Source_Capabilities update followed by Soft
                // Reset), the next ordinary SPR list proves that the port is
                // still in SPR. Do not leave the product controller stuck in
                // Entering while the policy engine has already recovered.
                if matches!(self.epr_state, EprState::Entering | EprState::Exiting) {
                    self.epr_state = EprState::Spr;
                    self.epr_capabilities = None;
                    self.desired = None;
                    self.epr_exit_ready = false;
                    self.epr_exit_preserves_voltage = false;
                    self.epr_entry_pending = false;
                    self.epr_entry_preserves_voltage = false;
                }
            }
            CapabilitiesKind::Epr => {
                self.epr_capabilities = Some(capabilities);
                if !matches!(self.epr_state, EprState::Exiting) {
                    self.epr_state = EprState::Epr;
                }
            }
        }
    }

    /// Select the contract to request when the policy engine evaluates a new
    /// capability message. With no user request, this always selects 5 V. If
    /// changed capabilities can no longer satisfy a retained user request,
    /// clear that intent and fall back to the mandatory fixed 5 V offer.
    pub fn request_for_capabilities(
        &mut self,
        capabilities: SourceCapabilities,
    ) -> Result<RequestPlan, ControllerError> {
        self.request_for_capabilities_with_contract(capabilities, None)
    }

    /// Evaluate capabilities with the still-confirmed contract supplied by
    /// `ContractTracker`. The controller retains policy, not duplicate
    /// contract truth.
    pub fn request_for_capabilities_with_contract(
        &mut self,
        capabilities: SourceCapabilities,
        active: Option<RequestPlan>,
    ) -> Result<RequestPlan, ControllerError> {
        self.observe_capabilities(capabilities);
        let mode = match capabilities.kind() {
            CapabilitiesKind::Spr => PortMode::Spr,
            CapabilitiesKind::Epr => PortMode::Epr,
        };

        if self.epr_entry_pending && matches!(capabilities.kind(), CapabilitiesKind::Epr) && self.desired.is_none() {
            self.epr_entry_pending = false;
            if self.epr_entry_preserves_voltage {
                if let Some(plan) = active.and_then(|active| self.continuity_request(&capabilities, active, mode)) {
                    self.epr_entry_preserves_voltage = false;
                    return Ok(plan.0);
                }
                self.epr_entry_preserves_voltage = false;
                if matches!(self.epr_entry_fallback, EprEntryFallback::Refuse) {
                    let safe_5v = Self::safe_5v_request();
                    let plan = self.plan(safe_5v, &capabilities, mode)?;
                    self.epr_state = EprState::Exiting;
                    self.epr_exit_ready = false;
                    self.epr_exit_fallback = EprExitFallback::Safe5V;
                    self.epr_exit_preserves_voltage = false;
                    self.desired = Some(safe_5v);
                    self.pending_error = Some(ControllerError::EprEntryRefused(EprEntryRefusal::CapabilitiesChanged));
                    return Ok(plan);
                }
            }
        }

        if let Some(request) = self.desired {
            if let Ok(plan) = self.plan(request, &capabilities, mode) {
                let continuity_valid = !matches!(self.epr_state, EprState::Exiting)
                    || !self.epr_exit_preserves_voltage
                    || active.is_some_and(|active| {
                        matches!(
                            ContractTransition::classify(Some(active), plan).kind,
                            ContractTransitionKind::IdenticalRefresh
                                | ContractTransitionKind::SameVoltageSufficientCurrent
                        )
                    });
                if continuity_valid {
                    return Ok(plan);
                }
            }
            if matches!(self.epr_state, EprState::Exiting) {
                let safe_5v =
                    UserRequest::Voltage { voltage: Millivolts(5_000), current: None, preference: Preference::Fixed };
                let plan = self.plan(safe_5v, &capabilities, mode)?;
                if matches!(self.epr_exit_fallback, EprExitFallback::Safe5V) {
                    self.desired = Some(safe_5v);
                    self.epr_exit_preserves_voltage = false;
                } else {
                    self.epr_state = EprState::Epr;
                    self.desired = None;
                    self.epr_exit_preserves_voltage = false;
                    self.pending_error = Some(ControllerError::EprExitRefused(EprExitRefusal::CapabilitiesChanged));
                }
                return Ok(plan);
            }
            self.desired = None;
        }

        self.planner
            .for_voltage(&capabilities, mode, Millivolts(5_000), None, Preference::Fixed, self.config.request_context)
            .map_err(Into::into)
    }

    /// Handle a user request against the currently retained source offers.
    /// If EPR capabilities are needed but not yet available, this returns the
    /// EPR-entry action and retains the request for automatic planning when
    /// EPR Source Capabilities arrive.
    pub fn submit(&mut self, request: UserRequest) -> Result<ControllerAction, ControllerError> {
        if matches!(self.epr_state, EprState::Entering | EprState::Exiting) {
            return Err(ControllerError::Busy(self.epr_state));
        }

        let (kind, mode) = match self.epr_state {
            EprState::Epr => (CapabilitiesKind::Epr, PortMode::Epr),
            EprState::Spr => (CapabilitiesKind::Spr, PortMode::Spr),
            EprState::Entering | EprState::Exiting => unreachable!(),
        };
        let capabilities = *self.capabilities(kind).ok_or(ControllerError::NoCapabilities(kind))?;

        match self.plan(request, &capabilities, mode) {
            Ok(plan) => {
                self.desired = Some(request);
                Ok(ControllerAction::Request(plan))
            }
            Err(ControllerError::Plan(error)) if self.should_try_epr(request, error) => self.begin_epr(Some(request)),
            Err(error) => Err(error),
        }
    }

    /// Plan a request against the currently retained capabilities without
    /// changing user intent, EPR state, or the active contract.
    pub fn preview(&self, request: UserRequest) -> Result<RequestPlan, ControllerError> {
        if matches!(self.epr_state, EprState::Entering | EprState::Exiting) {
            return Err(ControllerError::Busy(self.epr_state));
        }

        let (kind, mode) = match self.epr_state {
            EprState::Epr => (CapabilitiesKind::Epr, PortMode::Epr),
            EprState::Spr => (CapabilitiesKind::Spr, PortMode::Spr),
            EprState::Entering | EprState::Exiting => unreachable!(),
        };
        let capabilities = self.capabilities(kind).ok_or(ControllerError::NoCapabilities(kind))?;
        self.plan(request, capabilities, mode)
    }

    pub fn request_epr_capabilities(&self) -> Result<ControllerAction, ControllerError> {
        if matches!(self.epr_state, EprState::Epr) {
            Ok(ControllerAction::RequestEprCapabilities)
        } else {
            Err(ControllerError::NotInEprMode)
        }
    }

    /// Enter EPR only to learn the complete EPR Source Capabilities list.
    ///
    /// No user request is retained. The explicit policy selects whether the
    /// EPR capability response must re-express the confirmed SPR operating
    /// point, may fall back to fixed 5 V, or must refuse entry before any
    /// electrical transition begins.
    pub fn begin_epr_discovery(
        &mut self,
        policy: EprEntryPolicy,
        active: Option<RequestPlan>,
    ) -> Result<ControllerAction, ControllerError> {
        if !matches!(self.epr_state, EprState::Spr) {
            return Err(ControllerError::Busy(self.epr_state));
        }

        let capabilities =
            self.spr_capabilities.as_ref().ok_or(ControllerError::NoCapabilities(CapabilitiesKind::Spr))?;
        if !capabilities.epr_mode_capable() {
            return Err(ControllerError::EprUnavailable);
        }

        let (fallback, preserves_voltage) = match policy {
            EprEntryPolicy::Safe5V => (EprEntryFallback::Safe5V, false),
            EprEntryPolicy::PreserveVoltage { fallback } => {
                let active = active.ok_or(ControllerError::EprEntryRefused(EprEntryRefusal::NoConfirmedContract))?;
                if self.continuity_request(capabilities, active, PortMode::Spr).is_none()
                    && matches!(fallback, EprEntryFallback::Refuse)
                {
                    return Err(ControllerError::EprEntryRefused(EprEntryRefusal::NoSuitableSprContract));
                }
                (fallback, true)
            }
        };

        let action = self.begin_epr(None)?;
        self.epr_entry_pending = true;
        self.epr_entry_fallback = fallback;
        self.epr_entry_preserves_voltage = preserves_voltage;
        Ok(action)
    }

    /// Begin EPR exit using an explicit application policy and the confirmed
    /// contract owned by `ContractTracker`.
    pub fn exit_epr(
        &mut self,
        policy: EprExitPolicy,
        active: Option<RequestPlan>,
    ) -> Result<ControllerAction, ControllerError> {
        if !matches!(self.epr_state, EprState::Epr) {
            return Err(ControllerError::NotInEprMode);
        }

        let capabilities =
            self.epr_capabilities.as_ref().ok_or(ControllerError::NoCapabilities(CapabilitiesKind::Epr))?;
        let active_is_spr = active.is_some_and(|plan| {
            plan.message == RequestMessage::EprRequest
                && plan.object_position <= 7
                && matches!(plan.supply, SupplyKind::Fixed | SupplyKind::Pps | SupplyKind::SprAvs)
                && self.continuity_request_at(capabilities, plan, plan.object_position, PortMode::Epr).is_some()
        });

        let (plan, request, fallback, preserves_voltage) = match policy {
            EprExitPolicy::PreserveVoltage { fallback } => {
                let active = active.ok_or(ControllerError::EprExitRefused(EprExitRefusal::NoConfirmedContract))?;
                if active_is_spr {
                    self.start_direct_epr_exit();
                    return Ok(ControllerAction::ExitEprMode);
                }
                match self.continuity_request(capabilities, active, PortMode::Epr) {
                    Some((plan, request)) => (plan, request, fallback, true),
                    None if matches!(fallback, EprExitFallback::Refuse) => {
                        return Err(ControllerError::EprExitRefused(EprExitRefusal::NoSuitableSprContract));
                    }
                    None => {
                        let request = Self::safe_5v_request();
                        (self.plan(request, capabilities, PortMode::Epr)?, request, fallback, false)
                    }
                }
            }
            EprExitPolicy::Safe5V => {
                if active_is_spr
                    && active.is_some_and(|plan| {
                        plan.supply == SupplyKind::Fixed && plan.encoded_voltage() == Millivolts(5_000)
                    })
                {
                    self.start_direct_epr_exit();
                    return Ok(ControllerAction::ExitEprMode);
                }
                let request = Self::safe_5v_request();
                (self.plan(request, capabilities, PortMode::Epr)?, request, EprExitFallback::Safe5V, false)
            }
        };

        self.epr_state = EprState::Exiting;
        self.epr_exit_ready = false;
        self.epr_exit_fallback = fallback;
        self.epr_exit_preserves_voltage = preserves_voltage;
        self.desired = Some(request);
        Ok(ControllerAction::Request(plan))
    }

    /// Record that a requested transition has reached PS_RDY.
    pub fn on_ps_ready(&mut self) {
        if matches!(self.epr_state, EprState::Exiting) {
            self.epr_exit_ready = true;
        }
    }

    /// Return an internally generated action that should run before waiting
    /// for another user command.
    pub fn take_ready_action(&mut self) -> Option<ControllerAction> {
        if matches!(self.epr_state, EprState::Exiting) && self.epr_exit_ready {
            self.epr_exit_ready = false;
            self.desired = None;
            self.epr_exit_preserves_voltage = false;
            Some(ControllerAction::ExitEprMode)
        } else {
            None
        }
    }

    /// Restore EPR-ready state if the SPR contract needed for EPR exit was
    /// rejected. A fresh `exit-epr` command is required after a rejection.
    pub fn request_rejected(&mut self) {
        if matches!(self.epr_state, EprState::Exiting) {
            self.epr_state = EprState::Epr;
            self.epr_exit_ready = false;
            self.desired = None;
            self.epr_exit_preserves_voltage = false;
        }
    }

    /// Keep the EPR exit sequence armed when its SPR request was deferred.
    /// The policy engine will re-run request planning after SinkRequestTimer.
    pub fn request_deferred(&mut self) {
        if matches!(self.epr_state, EprState::Exiting) {
            self.epr_exit_ready = false;
        }
    }

    pub fn epr_entry_failed(&mut self) {
        self.epr_state = EprState::Spr;
        self.epr_exit_ready = false;
        self.epr_exit_fallback = EprExitFallback::Refuse;
        self.epr_exit_preserves_voltage = false;
        self.epr_entry_pending = false;
        self.epr_entry_fallback = EprEntryFallback::Refuse;
        self.epr_entry_preserves_voltage = false;
        self.pending_error = None;
        self.epr_capabilities = None;
        self.desired = None;
    }

    pub fn reset_port(&mut self) {
        self.spr_capabilities = None;
        self.epr_capabilities = None;
        self.epr_state = EprState::Spr;
        self.epr_exit_ready = false;
        self.epr_exit_fallback = EprExitFallback::Refuse;
        self.epr_exit_preserves_voltage = false;
        self.epr_entry_pending = false;
        self.epr_entry_fallback = EprEntryFallback::Refuse;
        self.epr_entry_preserves_voltage = false;
        self.pending_error = None;
        self.desired = None;
        self.config.request_context.source_present_pdp = None;
    }

    fn plan(
        &self,
        request: UserRequest,
        capabilities: &SourceCapabilities,
        mode: PortMode,
    ) -> Result<RequestPlan, ControllerError> {
        match request {
            UserRequest::Voltage { voltage, current, preference } => {
                self.planner.for_voltage(capabilities, mode, voltage, current, preference, self.config.request_context)
            }
            UserRequest::Pdo { position, demand } => {
                self.planner.for_pdo(capabilities, mode, position, demand, self.config.request_context)
            }
        }
        .map_err(Into::into)
    }

    fn should_try_epr(&self, request: UserRequest, error: PlanError) -> bool {
        if !matches!(self.epr_state, EprState::Spr)
            || !self.spr_capabilities.is_some_and(|capabilities| capabilities.epr_mode_capable())
        {
            return false;
        }

        match (request, error) {
            (_, PlanError::EprModeRequired(_)) => true,
            (UserRequest::Pdo { position, .. }, PlanError::PositionUnavailable(_)) => position >= 8,
            (
                UserRequest::Voltage { voltage, preference, .. },
                PlanError::VoltageUnavailable(_) | PlanError::PositionUnavailable(_),
            ) => voltage >= Millivolts(15_000) || matches!(preference, Preference::EprAvsNonstandard),
            _ => false,
        }
    }

    fn begin_epr(&mut self, desired: Option<UserRequest>) -> Result<ControllerAction, ControllerError> {
        let pdp = self.config.epr_operational_pdp.ok_or(ControllerError::EprNotConfigured)?;
        if pdp < Milliwatts(1_000) || pdp > Milliwatts(240_000) || pdp.get() % 1_000 != 0 {
            return Err(ControllerError::InvalidEprOperationalPdp(pdp));
        }

        if desired.is_some() {
            self.epr_entry_pending = false;
            self.epr_entry_preserves_voltage = false;
        }
        self.desired = desired;
        self.epr_state = EprState::Entering;
        Ok(ControllerAction::EnterEprMode { operational_pdp: pdp })
    }

    fn safe_5v_request() -> UserRequest {
        UserRequest::Voltage { voltage: Millivolts(5_000), current: None, preference: Preference::Fixed }
    }

    fn start_direct_epr_exit(&mut self) {
        self.epr_state = EprState::Exiting;
        self.epr_exit_ready = false;
        self.epr_exit_preserves_voltage = false;
        self.desired = None;
    }

    fn continuity_request(
        &self,
        capabilities: &SourceCapabilities,
        active: RequestPlan,
        mode: PortMode,
    ) -> Option<(RequestPlan, UserRequest)> {
        for position in 1..=7 {
            if let Some(candidate) = self.continuity_request_at(capabilities, active, position, mode) {
                return Some(candidate);
            }
        }
        None
    }

    fn continuity_request_at(
        &self,
        capabilities: &SourceCapabilities,
        active: RequestPlan,
        position: u8,
        mode: PortMode,
    ) -> Option<(RequestPlan, UserRequest)> {
        let request = UserRequest::Voltage {
            voltage: active.encoded_voltage(),
            current: active.operating_current(),
            preference: Preference::Position(position),
        };
        let plan = self.plan(request, capabilities, mode).ok()?;
        matches!(
            ContractTransition::classify(Some(active), plan).kind,
            ContractTransitionKind::IdenticalRefresh | ContractTransitionKind::SameVoltageSufficientCurrent
        )
        .then_some((plan, request))
    }
}
