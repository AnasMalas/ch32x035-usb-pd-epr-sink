//! Optional application-side arbitration for PD and user load control.
//!
//! This state owns no GPIO and performs no voltage measurement. It only
//! combines library load-policy updates without conflating the absence of a
//! usable PD session with a safety inhibit from an active PD session.

use crate::{ContractTransition, TransitionLoadPolicy};

/// Latched PD input to an application-owned load supervisor.
///
/// Applications remain responsible for their user latch and board-specific
/// VBUS-valid, health, and hardware cutoff paths. This helper makes the PD
/// portion explicit and keeps a required user-latch clear sticky until the
/// supervisor consumes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadControlState {
    policy_active: bool,
    permitted: bool,
    uninterrupted_transition: bool,
    clear_user_latch: bool,
}

impl LoadControlState {
    /// No usable PD session currently controls the product load.
    pub const fn unmanaged() -> Self {
        Self { policy_active: false, permitted: false, uninterrupted_transition: false, clear_user_latch: false }
    }

    /// Apply an ordinary PD permission or unconditional active-session
    /// inhibit. A `false` update while unmanaged does not claim ownership of a
    /// non-PD load.
    pub fn set_pd_permitted(&mut self, permitted: bool) {
        self.permitted = permitted;
        if permitted {
            self.policy_active = true;
            self.uninterrupted_transition = false;
        } else if self.policy_active {
            self.uninterrupted_transition = false;
            self.clear_user_latch = true;
        }
    }

    /// Apply one classified Request's configured load-continuity policy.
    pub fn begin_transition(&mut self, policy: TransitionLoadPolicy, transition: ContractTransition) {
        self.policy_active = true;
        if !transition.inhibits_load() {
            return;
        }

        match policy {
            TransitionLoadPolicy::InhibitUntilManualRearm => {
                self.permitted = false;
                self.uninterrupted_transition = false;
                self.clear_user_latch = true;
            }
            TransitionLoadPolicy::InhibitUntilReady => {
                self.permitted = false;
                self.uninterrupted_transition = false;
            }
            TransitionLoadPolicy::Uninterrupted => {
                self.uninterrupted_transition = true;
            }
        }
    }

    /// Release PD ownership after an initial/uncontracted partner is known to
    /// be unavailable. A pending safety clear remains latched.
    pub fn set_unmanaged(&mut self) {
        let clear_user_latch = self.clear_user_latch;
        *self = Self { clear_user_latch, ..Self::unmanaged() };
    }

    /// Whether PD currently constrains the product output.
    pub const fn policy_active(&self) -> bool {
        self.policy_active
    }

    /// Whether a confirmed PD contract currently permits the output.
    pub const fn pd_permitted(&self) -> bool {
        self.permitted
    }

    /// Whether the selected policy deliberately bypasses transition
    /// inhibition for a suitably rated downstream load.
    pub const fn uninterrupted_transition(&self) -> bool {
        self.uninterrupted_transition
    }

    /// PD's input to the application's final load-enable equation.
    pub const fn pd_allows_load(&self) -> bool {
        !self.policy_active || self.permitted || self.uninterrupted_transition
    }

    /// Consume a sticky request to clear the application user latch.
    pub fn take_user_latch_clear(&mut self) -> bool {
        let clear = self.clear_user_latch;
        self.clear_user_latch = false;
        clear
    }
}

impl Default for LoadControlState {
    fn default() -> Self {
        Self::unmanaged()
    }
}

#[cfg(test)]
mod tests {
    use crate::{ContractOperatingPoint, ContractTransitionKind, Milliamps, Millivolts};

    use super::*;

    fn transition(kind: ContractTransitionKind) -> ContractTransition {
        ContractTransition {
            kind,
            from: Some(ContractOperatingPoint { voltage: Millivolts(5_000), current: Milliamps(2_000) }),
            to: ContractOperatingPoint { voltage: Millivolts(9_000), current: Milliamps(2_000) },
        }
    }

    #[test]
    fn unmanaged_non_pd_power_ignores_repeated_pd_denials() {
        let mut state = LoadControlState::unmanaged();
        state.set_pd_permitted(false);
        state.set_pd_permitted(false);

        assert!(state.pd_allows_load());
        assert!(!state.policy_active());
        assert!(!state.take_user_latch_clear());
    }

    #[test]
    fn automatic_transition_restore_preserves_the_user_latch() {
        let mut state = LoadControlState::unmanaged();
        state.begin_transition(
            TransitionLoadPolicy::InhibitUntilReady,
            transition(ContractTransitionKind::VoltageChange),
        );
        assert!(!state.pd_allows_load());
        assert!(!state.take_user_latch_clear());

        state.set_pd_permitted(true);
        assert!(state.pd_allows_load());
    }

    #[test]
    fn safety_clear_survives_a_later_unmanaged_update() {
        let mut state = LoadControlState::unmanaged();
        state.set_pd_permitted(true);
        state.set_pd_permitted(false);
        state.set_unmanaged();

        assert!(state.pd_allows_load());
        assert!(state.take_user_latch_clear());
        assert!(!state.take_user_latch_clear());
    }

    #[test]
    fn uninterrupted_policy_never_weakens_a_later_fault() {
        let mut state = LoadControlState::unmanaged();
        state.begin_transition(TransitionLoadPolicy::Uninterrupted, transition(ContractTransitionKind::VoltageChange));
        assert!(state.pd_allows_load());

        state.set_pd_permitted(false);
        assert!(!state.pd_allows_load());
        assert!(state.take_user_latch_clear());
    }

    #[test]
    fn manual_rearm_is_distinct_from_automatic_restore() {
        let mut state = LoadControlState::unmanaged();
        state.begin_transition(
            TransitionLoadPolicy::InhibitUntilManualRearm,
            transition(ContractTransitionKind::VoltageChange),
        );

        assert!(!state.pd_allows_load());
        assert!(state.take_user_latch_clear());
    }
}
