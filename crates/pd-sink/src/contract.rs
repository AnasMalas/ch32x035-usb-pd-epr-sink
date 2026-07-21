use crate::request::{PlannedOperating, RequestPlan};
use crate::units::Milliamps;

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

    /// Whether starting this request changes any wire-encoded operating
    /// parameter from the confirmed contract. Identical PPS maintenance
    /// requests do not require the product load to be interrupted.
    pub fn request_changes_power(&self, plan: RequestPlan) -> bool {
        match self.active {
            Some(active) => {
                active.message != plan.message || active.rdo != plan.rdo || active.pdo_copy != plan.pdo_copy
            }
            None => true,
        }
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
            Some(RequestPlan { operating: PlannedOperating::Current { operating, .. }, .. }) => Some(operating),
            _ => None,
        }
    }
}
