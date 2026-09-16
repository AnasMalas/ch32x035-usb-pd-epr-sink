use crate::request::{CurrentConfidence, PlannedOperating, RequestPlan};
use crate::units::{Milliamps, Millivolts};

/// Wire-level operating point used to explain a contract transition without
/// making applications retain or compare complete PDO/RDO plans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContractOperatingPoint {
    pub voltage: Millivolts,
    pub current: Milliamps,
}

/// Conservative local classification made before a PD Request is sent.
///
/// The Source does not provide an application-load safety classification.
/// This library compares the new request with the confirmed RDO so firmware
/// can inhibit its external load before an electrical transition starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ContractTransitionKind {
    NoConfirmedContract = 0,
    IdenticalRefresh = 1,
    SameVoltageSufficientCurrent = 2,
    SameVoltageReducedCurrent = 3,
    SameVoltageUnknownCurrent = 4,
    VoltageChange = 5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContractTransition {
    pub kind: ContractTransitionKind,
    pub from: Option<ContractOperatingPoint>,
    pub to: ContractOperatingPoint,
}

impl ContractTransition {
    pub fn classify(active: Option<RequestPlan>, plan: RequestPlan) -> Self {
        let to = ContractOperatingPoint { voltage: plan.encoded_voltage(), current: plan.operating_current() };
        let Some(active) = active else {
            return Self { kind: ContractTransitionKind::NoConfirmedContract, from: None, to };
        };
        let from = ContractOperatingPoint { voltage: active.encoded_voltage(), current: active.operating_current() };

        let kind =
            if active.message() == plan.message() && active.rdo() == plan.rdo() && active.pdo_copy() == plan.pdo_copy()
            {
                ContractTransitionKind::IdenticalRefresh
            } else if from.voltage != to.voltage {
                ContractTransitionKind::VoltageChange
            } else if matches!(
                plan.operating(),
                PlannedOperating::Current { confidence: CurrentConfidence::PowerLimitedUpperBound, .. }
            ) {
                ContractTransitionKind::SameVoltageUnknownCurrent
            } else if to.current >= from.current {
                ContractTransitionKind::SameVoltageSufficientCurrent
            } else {
                ContractTransitionKind::SameVoltageReducedCurrent
            };

        Self { kind, from: Some(from), to }
    }

    /// Whether the application load must be inhibited before sending the
    /// Request. Observation/telemetry of this decision is never on the safety
    /// path.
    pub const fn inhibits_load(self) -> bool {
        matches!(
            self.kind,
            ContractTransitionKind::NoConfirmedContract
                | ContractTransitionKind::SameVoltageReducedCurrent
                | ContractTransitionKind::SameVoltageUnknownCurrent
                | ContractTransitionKind::VoltageChange
        )
    }

    pub const fn is_identical_refresh(self) -> bool {
        matches!(self.kind, ContractTransitionKind::IdenticalRefresh)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContractState {
    #[default]
    Detached,
    Attached,
    Advertised,
    Pending,
    Accepted,
    Ready,
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContractError {
    NotAttached,
    NoCapabilities,
    NoPendingRequest,
    RequestNotAccepted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContractTracker {
    state: ContractState,
    active: Option<RequestPlan>,
    pending: Option<RequestPlan>,
}

impl ContractTracker {
    pub const fn new() -> Self {
        Self { state: ContractState::Detached, active: None, pending: None }
    }

    pub const fn state(&self) -> ContractState {
        self.state
    }

    pub const fn pending_or_active_plan(&self) -> Option<RequestPlan> {
        match self.pending {
            Some(plan) => Some(plan),
            None => self.active,
        }
    }

    pub const fn active_plan(&self) -> Option<RequestPlan> {
        self.active
    }

    /// Classify a request against the current confirmed RDO.
    ///
    /// Current comparisons use the limited, wire-encoded operating current,
    /// not the Source PDO maximum or the user's pre-limit demand. A
    /// power-limited upper bound is not a confirmed current capability and is
    /// therefore treated conservatively as unknown.
    pub fn classify_transition(&self, plan: RequestPlan) -> ContractTransition {
        ContractTransition::classify(self.active, plan)
    }

    pub fn on_attach(&mut self) {
        self.state = ContractState::Attached;
        self.active = None;
        self.pending = None;
    }

    pub fn on_capabilities(&mut self) -> Result<(), ContractError> {
        if matches!(self.state, ContractState::Detached) {
            return Err(ContractError::NotAttached);
        }
        self.state = ContractState::Advertised;
        self.pending = None;
        Ok(())
    }

    pub fn on_request(&mut self, plan: RequestPlan) -> Result<(), ContractError> {
        if !matches!(self.state, ContractState::Advertised | ContractState::Ready) {
            return Err(ContractError::NoCapabilities);
        }
        self.state = ContractState::Pending;
        self.pending = Some(plan);
        Ok(())
    }

    pub fn on_accept(&mut self) -> Result<(), ContractError> {
        if !matches!(self.state, ContractState::Pending) {
            return Err(ContractError::NoPendingRequest);
        }
        self.state = ContractState::Accepted;
        Ok(())
    }

    pub fn on_ps_ready(&mut self) -> Result<(), ContractError> {
        if !matches!(self.state, ContractState::Accepted) {
            return Err(ContractError::RequestNotAccepted);
        }
        self.active = self.pending.take();
        self.state = ContractState::Ready;
        Ok(())
    }

    pub fn on_reject_or_wait(&mut self) {
        if self.active.is_some() {
            self.state = ContractState::Ready;
        } else if !matches!(self.state, ContractState::Detached) {
            self.state = ContractState::Advertised;
        }
        self.pending = None;
    }

    pub fn on_protocol_loss(&mut self) {
        if !matches!(self.state, ContractState::Detached) {
            self.state = ContractState::Lost;
        }
        self.active = None;
        self.pending = None;
    }

    pub fn on_detach(&mut self) {
        self.state = ContractState::Detached;
        self.active = None;
        self.pending = None;
    }

    pub const fn load_may_enable(&self) -> bool {
        matches!(self.state, ContractState::Ready)
    }

    pub const fn confirmed_current(&self) -> Option<Milliamps> {
        if !matches!(self.state, ContractState::Ready) {
            return None;
        }
        match self.active {
            Some(plan) => Some(plan.operating_current()),
            None => None,
        }
    }
}
