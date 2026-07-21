#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SafetyTimings {
    /// Stable attach interval before starting PD communication.
    pub attach_debounce_ms: u32,
    /// Stable absence interval before rebuilding the complete port stack.
    /// Load cutoff does not wait for this timer.
    pub detach_confirm_ms: u32,
}

impl Default for SafetyTimings {
    fn default() -> Self {
        Self { attach_debounce_ms: 100, detach_confirm_ms: 5 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortInputs {
    pub cc_attached: bool,
    pub vbus_present: bool,
    /// False for an over-voltage, over-temperature, power-path, or other local
    /// hardware fault.
    pub hardware_ok: bool,
}

impl PortInputs {
    pub const fn connected(self) -> bool {
        self.cc_attached && self.vbus_present && self.hardware_ok
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PortState {
    #[default]
    Unattached,
    AttachDebounce {
        since_ms: u32,
    },
    Attached,
    DetachDebounce {
        since_ms: u32,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SafetyDecision {
    pub state: PortState,
    /// The firmware-controlled load output may be asserted. A separate
    /// hardware cutoff must enforce cable-removal safety independently.
    pub load_enable: bool,
    /// Deassert the load output immediately in this update.
    pub force_cutoff: bool,
    pub attach_confirmed: bool,
    pub detach_confirmed: bool,
    pub restart_pd_stack: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortSupervisor {
    timings: SafetyTimings,
    state: PortState,
    contract_ready: bool,
    load_enabled: bool,
}

impl PortSupervisor {
    pub const fn new(timings: SafetyTimings) -> Self {
        Self { timings, state: PortState::Unattached, contract_ready: false, load_enabled: false }
    }

    pub const fn state(&self) -> PortState {
        self.state
    }

    pub const fn load_may_enable(&self) -> bool {
        self.load_enabled
    }

    /// Mark the negotiated contract ready after Accept and PS_RDY.
    ///
    /// Returns whether the load may now be enabled. It remains false unless
    /// attach has completed its debounce interval.
    pub fn mark_contract_ready(&mut self) -> bool {
        self.contract_ready = true;
        self.load_enabled = matches!(self.state, PortState::Attached);
        self.load_enabled
    }

    /// Invalidate the contract on reset, timeout, policy failure, or detach.
    /// Returns true when a previously enabled load must be cut off now.
    pub fn invalidate_contract(&mut self) -> bool {
        let cutoff = self.load_enabled;
        self.contract_ready = false;
        self.load_enabled = false;
        cutoff
    }

    pub fn update(&mut self, now_ms: u32, inputs: PortInputs) -> SafetyDecision {
        let was_load_enabled = self.load_enabled;
        let connected = inputs.connected();
        let mut attach_confirmed = false;
        let mut detach_confirmed = false;
        let mut restart_pd_stack = false;

        self.state = match self.state {
            PortState::Unattached if connected => PortState::AttachDebounce { since_ms: now_ms },
            PortState::Unattached => PortState::Unattached,
            PortState::AttachDebounce { .. } if !connected => PortState::Unattached,
            PortState::AttachDebounce { since_ms }
                if now_ms.wrapping_sub(since_ms) >= self.timings.attach_debounce_ms =>
            {
                attach_confirmed = true;
                PortState::Attached
            }
            PortState::AttachDebounce { since_ms } => PortState::AttachDebounce { since_ms },
            PortState::Attached if !connected => {
                self.contract_ready = false;
                PortState::DetachDebounce { since_ms: now_ms }
            }
            PortState::Attached => PortState::Attached,
            PortState::DetachDebounce { since_ms }
                if !connected && now_ms.wrapping_sub(since_ms) >= self.timings.detach_confirm_ms =>
            {
                detach_confirmed = true;
                restart_pd_stack = true;
                PortState::Unattached
            }
            // A disappearing signal always invalidates the contract. If it
            // returns during debounce, require a fresh stable attach instead
            // of silently restoring the old contract.
            PortState::DetachDebounce { .. } if connected => PortState::AttachDebounce { since_ms: now_ms },
            PortState::DetachDebounce { since_ms } => PortState::DetachDebounce { since_ms },
        };

        self.load_enabled = connected && self.contract_ready && matches!(self.state, PortState::Attached);

        SafetyDecision {
            state: self.state,
            load_enable: self.load_enabled,
            force_cutoff: was_load_enabled && !self.load_enabled,
            attach_confirmed,
            detach_confirmed,
            restart_pd_stack,
        }
    }
}
