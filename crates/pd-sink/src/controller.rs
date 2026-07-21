use crate::capabilities::{CapabilitiesKind, SourceCapabilities};
use crate::request::{Demand, PlanError, PortMode, Preference, RequestContext, RequestPlan, RequestPlanner};
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
            desired: None,
        }
    }

    pub const fn epr_state(&self) -> EprState {
        self.epr_state
    }

    pub const fn desired(&self) -> Option<UserRequest> {
        self.desired
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
        self.observe_capabilities(capabilities);
        let mode = match capabilities.kind() {
            CapabilitiesKind::Spr => PortMode::Spr,
            CapabilitiesKind::Epr => PortMode::Epr,
        };

        if let Some(request) = self.desired {
            if let Ok(plan) = self.plan(request, &capabilities, mode) {
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
    /// No user request is retained, so evaluation of the EPR capabilities
    /// deliberately selects the required fixed 5 V PDO. This makes discovery
    /// safe to run automatically after the initial SPR contract is ready.
    pub fn begin_epr_discovery(&mut self) -> Result<ControllerAction, ControllerError> {
        if !matches!(self.epr_state, EprState::Spr) {
            return Err(ControllerError::Busy(self.epr_state));
        }

        let capabilities =
            self.spr_capabilities.as_ref().ok_or(ControllerError::NoCapabilities(CapabilitiesKind::Spr))?;
        if !capabilities.epr_mode_capable() {
            return Err(ControllerError::EprUnavailable);
        }

        self.begin_epr(None)
    }

    /// Begin the required two-step EPR exit sequence.
    ///
    /// A Sink first establishes an SPR PDO contract while still using an
    /// EPR_Request message. Only after that request reaches PS_RDY may it send
    /// EPR_Mode Exit. The returned request deliberately selects fixed 5 V.
    pub fn exit_epr(&mut self) -> Result<ControllerAction, ControllerError> {
        if !matches!(self.epr_state, EprState::Epr) {
            return Err(ControllerError::NotInEprMode);
        }

        let capabilities =
            self.epr_capabilities.as_ref().ok_or(ControllerError::NoCapabilities(CapabilitiesKind::Epr))?;
        let plan = self.planner.for_voltage(
            capabilities,
            PortMode::Epr,
            Millivolts(5_000),
            None,
            Preference::Fixed,
            self.config.request_context,
        )?;

        self.epr_state = EprState::Exiting;
        self.epr_exit_ready = false;
        self.desired = None;
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
        self.epr_capabilities = None;
        self.desired = None;
    }

    pub fn reset_port(&mut self) {
        self.spr_capabilities = None;
        self.epr_capabilities = None;
        self.epr_state = EprState::Spr;
        self.epr_exit_ready = false;
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

        self.desired = desired;
        self.epr_state = EprState::Entering;
        Ok(ControllerAction::EnterEprMode { operational_pdp: pdp })
    }
}
