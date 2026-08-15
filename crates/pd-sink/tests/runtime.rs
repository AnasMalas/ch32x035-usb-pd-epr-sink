use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};
use std::collections::VecDeque;

use pd_sink::{
    CapabilitiesKind, Command, ContractState, ContractTransitionKind, ControllerConfig, HardResetCause,
    HardResetDirection, Milliamps, Millivolts, Milliwatts, PortMode, Preference, RecoveryCancellationReason,
    RecoveryInitError, RecoveryIntent, RequestContext, RequestFlags, SinkConfig, SinkConfigError, SinkDevice,
    SinkEvent, SinkPowerDescriptor, SinkRuntime, UserRequest,
};
use usbpd::protocol_layer::message::data::alert::AlertDataObject;
use usbpd::protocol_layer::message::data::request::PowerSource;
use usbpd::protocol_layer::message::data::source_capabilities::{
    Augmented, FixedSupply, PowerDataObject, SourceCapabilities, SprProgrammablePowerSupply,
};
use usbpd::protocol_layer::message::extended::pps_status::PpsStatus as StackPpsStatus;
use usbpd::sink::device_policy_manager::{
    DevicePolicyManager, Event, HardResetOrigin, HardResetReason, SinkStartup, SoftResetMode,
};

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

#[derive(Default)]
struct TestRuntime {
    events: Vec<SinkEvent>,
    load_states: Vec<bool>,
    user_output_states: Vec<bool>,
    commands: VecDeque<Command>,
    clear_count: usize,
    delays: Vec<u64>,
    cancel_recovery: bool,
}

impl SinkRuntime for TestRuntime {
    fn set_pd_load_permitted(&mut self, permitted: bool) {
        self.load_states.push(permitted);
    }

    fn set_user_output_enabled(&mut self, enabled: bool) {
        self.user_output_states.push(enabled);
    }

    fn clear_pending_commands(&mut self) {
        self.clear_count += 1;
    }

    fn recovery_cancel_requested(&self) -> bool {
        self.cancel_recovery
    }

    fn observe(&mut self, event: SinkEvent) {
        self.events.push(event);
    }

    async fn wait_for_command(&mut self) -> Command {
        self.commands.pop_front().expect("test must queue every command before polling")
    }

    async fn delay_millis(&mut self, milliseconds: u64) {
        self.delays.push(milliseconds);
    }
}

fn safe_5v_config() -> SinkConfig {
    SinkConfig {
        controller: ControllerConfig::default(),
        descriptor: SinkPowerDescriptor {
            vendor_id: 0x1209,
            product_id: 0x0001,
            maximum_current: Milliamps(3_000),
            pps_supported: false,
            avs_supported: false,
            spr_minimum_pdp_watts: 5,
            spr_operational_pdp_watts: 15,
            spr_maximum_pdp_watts: 15,
            epr_minimum_pdp_watts: 0,
            epr_operational_pdp_watts: 0,
            epr_maximum_pdp_watts: 0,
        },
        max_auto_epr_attempts: 0,
        hard_reset_recovery_ms: 2_000,
    }
}

fn recovery_intent(maximum_attempts: u8) -> RecoveryIntent {
    RecoveryIntent {
        mode: PortMode::Spr,
        request: UserRequest::Voltage {
            voltage: Millivolts(5_000),
            current: Some(Milliamps(2_000)),
            preference: Preference::Fixed,
        },
        restore_output: true,
        maximum_attempts,
    }
}

#[test]
fn fresh_startup_remains_the_default() {
    let device = SinkDevice::new(safe_5v_config(), TestRuntime::default()).unwrap();

    assert_eq!(DevicePolicyManager::startup(&device), SinkStartup::Fresh);
}

#[test]
fn warm_spr_recovery_keeps_both_load_controls_off_until_ps_rdy() {
    let intent = recovery_intent(2);
    let mut device = SinkDevice::new_recovering(safe_5v_config(), TestRuntime::default(), intent).unwrap();
    assert_eq!(DevicePolicyManager::startup(&device), SinkStartup::SoftReset(SoftResetMode::Spr));
    assert_eq!(device.recovery_intent(), Some(intent));
    assert_eq!(device.runtime_mut().load_states, [false]);
    assert_eq!(device.runtime_mut().user_output_states, [false]);
    assert!(device.runtime_mut().events.contains(&SinkEvent::RecoveryStarted(intent)));

    let source = SourceCapabilities::new_vsafe5v_only(500);
    device.inform(&source);
    let request = device.request(&source);
    assert_eq!(device.recovery_attempts(), 1);
    assert_eq!(device.contract().confirmed_current(), None);
    assert_eq!(device.contract().pending_or_active_plan().unwrap().operating_current(), Some(Milliamps(2_000)));
    assert!(device.runtime_mut().load_states.iter().all(|permitted| !permitted));
    assert_eq!(device.runtime_mut().user_output_states, [false]);

    device.transition_power(&request);

    assert_eq!(device.recovery_intent(), None);
    assert_eq!(device.contract().confirmed_current(), Some(Milliamps(2_000)));
    let runtime = device.runtime_mut();
    assert_eq!(runtime.load_states.last(), Some(&true));
    assert_eq!(runtime.user_output_states, [false, true]);
    assert!(runtime.events.iter().any(|event| matches!(
        event,
        SinkEvent::RecoverySucceeded { attempt: 1, plan, output_restored: true }
            if plan.operating_current() == Some(Milliamps(2_000))
    )));
}

#[test]
fn recovery_wait_retries_are_bounded_and_then_fall_back_without_output_restore() {
    let intent = recovery_intent(2);
    let source = SourceCapabilities::new_vsafe5v_only(500);
    let mut device = SinkDevice::new_recovering(safe_5v_config(), TestRuntime::default(), intent).unwrap();

    device.inform(&source);
    let _ = device.request(&source);
    device.request_not_accepted(usbpd::sink::device_policy_manager::RequestRejection::Wait);
    assert_eq!(device.recovery_attempts(), 1);
    assert!(device.runtime_mut().events.contains(&SinkEvent::RecoveryDeferred { attempt: 1, maximum_attempts: 2 }));

    device.inform(&source);
    let _ = device.request(&source);
    device.request_not_accepted(usbpd::sink::device_policy_manager::RequestRejection::Wait);
    assert_eq!(device.recovery_intent(), None);
    assert!(device.runtime_mut().events.contains(&SinkEvent::RecoveryCancelled {
        reason: RecoveryCancellationReason::AttemptsExhausted,
        attempts: 2,
        maximum_attempts: 2
    }));

    device.inform(&source);
    let _ = device.request(&source);
    assert_eq!(device.contract().pending_or_active_plan().unwrap().operating_current(), Some(Milliamps(5_000)));
    assert_eq!(device.runtime_mut().user_output_states, [false]);
    assert!(device.runtime_mut().load_states.iter().all(|permitted| !permitted));
}

#[test]
fn explicit_reject_cancels_recovery_without_retrying_the_same_target() {
    let source = SourceCapabilities::new_vsafe5v_only(500);
    let mut device = SinkDevice::new_recovering(safe_5v_config(), TestRuntime::default(), recovery_intent(3)).unwrap();

    device.inform(&source);
    let _ = device.request(&source);
    device.request_not_accepted(usbpd::sink::device_policy_manager::RequestRejection::Reject);

    assert_eq!(device.recovery_intent(), None);
    assert!(device.runtime_mut().events.contains(&SinkEvent::RecoveryCancelled {
        reason: RecoveryCancellationReason::RequestRejected,
        attempts: 1,
        maximum_attempts: 3
    }));
}

#[test]
fn out_of_band_output_off_cannot_be_undone_by_recovery_ps_rdy() {
    let source = SourceCapabilities::new_vsafe5v_only(500);
    let mut device = SinkDevice::new_recovering(safe_5v_config(), TestRuntime::default(), recovery_intent(2)).unwrap();
    device.inform(&source);
    let request = device.request(&source);

    device.runtime_mut().cancel_recovery = true;
    device.runtime_mut().set_user_output_enabled(false);
    device.transition_power(&request);

    let runtime = device.runtime_mut();
    assert_eq!(runtime.load_states.last(), Some(&true), "the newly confirmed PD contract remains usable");
    assert!(runtime.user_output_states.iter().all(|enabled| !enabled));
    assert!(runtime.events.contains(&SinkEvent::RecoveryCancelled {
        reason: RecoveryCancellationReason::ApplicationRequested,
        attempts: 1,
        maximum_attempts: 2
    }));
    assert!(!runtime.events.iter().any(|event| matches!(event, SinkEvent::RecoverySucceeded { .. })));
}

#[test]
fn in_band_output_off_cancels_recovery_without_submitting_a_pd_request() {
    let source = SourceCapabilities::new_vsafe5v_only(500);
    let mut device = SinkDevice::new_recovering(safe_5v_config(), TestRuntime::default(), recovery_intent(2)).unwrap();
    device.runtime_mut().commands.push_back(Command::OutputOff);
    device.runtime_mut().commands.push_back(Command::RequestSourceStatus);

    assert!(matches!(block_on(device.get_event(&source)), Event::RequestStatus));
    assert_eq!(device.recovery_intent(), None);
    let runtime = device.runtime_mut();
    assert_eq!(runtime.user_output_states, [false, false]);
    assert!(runtime.events.contains(&SinkEvent::RecoveryCancelled {
        reason: RecoveryCancellationReason::ApplicationRequested,
        attempts: 0,
        maximum_attempts: 2
    }));
    assert!(!runtime.events.iter().any(|event| matches!(event, SinkEvent::Requesting(_))));
}

#[test]
fn every_session_loss_path_cancels_recovery() {
    let cases = [
        (RecoveryCancellationReason::HardReset, 0_u8),
        (RecoveryCancellationReason::Detached, 1),
        (RecoveryCancellationReason::ProtocolLost, 2),
    ];

    for (expected, operation) in cases {
        let mut device =
            SinkDevice::new_recovering(safe_5v_config(), TestRuntime::default(), recovery_intent(2)).unwrap();
        match operation {
            0 => device.hard_reset(HardResetOrigin::Source, HardResetReason::SourceSignaled),
            1 => device.detached(),
            2 => device.protocol_lost(),
            _ => unreachable!(),
        }

        assert_eq!(device.recovery_intent(), None);
        assert!(device.runtime_mut().events.contains(&SinkEvent::RecoveryCancelled {
            reason: expected,
            attempts: 0,
            maximum_attempts: 2
        }));
        assert!(device.runtime_mut().user_output_states.iter().all(|enabled| !enabled));
        assert!(device.runtime_mut().load_states.iter().all(|permitted| !permitted));
    }
}

#[test]
fn epr_recovery_selects_epr_startup_and_an_epr_request() {
    let mut config = safe_5v_config();
    config.controller = ControllerConfig {
        request_context: RequestContext {
            flags: RequestFlags { epr_capable: true, ..RequestFlags::default() },
            ..RequestContext::default()
        },
        epr_operational_pdp: Some(Milliwatts(140_000)),
    };
    config.descriptor.pps_supported = true;
    config.descriptor.avs_supported = true;
    config.descriptor.epr_minimum_pdp_watts = 5;
    config.descriptor.epr_operational_pdp_watts = 140;
    config.descriptor.epr_maximum_pdp_watts = 140;

    let fixed_5v = ((5_000_u32 / 50) << 10) | (3_000 / 10) | (1 << 23);
    let fixed_48v = ((48_000_u32 / 50) << 10) | (5_000 / 10);
    let mut raw_pdos = heapless::Vec::new();
    raw_pdos.push(fixed_5v).unwrap();
    for _ in 1..7 {
        raw_pdos.push(0).unwrap();
    }
    raw_pdos.push(fixed_48v).unwrap();
    let source = SourceCapabilities::new_with_raw_pdos(raw_pdos);
    let intent = RecoveryIntent {
        mode: PortMode::Epr,
        request: UserRequest::Pdo { position: 8, demand: pd_sink::Demand::Maximum },
        restore_output: true,
        maximum_attempts: 1,
    };
    let mut device = SinkDevice::new_recovering(config, TestRuntime::default(), intent).unwrap();

    assert_eq!(DevicePolicyManager::startup(&device), SinkStartup::SoftReset(SoftResetMode::Epr));
    device.inform(&source);
    let request = device.request(&source);
    assert!(matches!(request, PowerSource::EprRequest(_)));
    assert_eq!(device.contract().pending_or_active_plan().unwrap().object_position, 8);
}

#[test]
fn reusable_device_owns_contract_and_hard_reset_safety() {
    let mut device = SinkDevice::new(safe_5v_config(), TestRuntime::default()).unwrap();
    let source = SourceCapabilities::new_vsafe5v_only(300);

    device.inform(&source);
    let request = device.request(&source);
    assert!(matches!(request, PowerSource::FixedVariableSupply(_)));
    assert_eq!(device.contract().state(), ContractState::Pending);

    device.transition_power(&request);
    assert_eq!(device.contract().state(), ContractState::Ready);
    assert_eq!(device.contract().active_plan().unwrap().object_position, 1);
    assert_eq!(device.runtime_mut().load_states.last(), Some(&true));

    device.hard_reset(HardResetOrigin::Source, HardResetReason::SourceSignaled);
    assert_eq!(device.contract().state(), ContractState::Lost);
    assert_eq!(DevicePolicyManager::hard_reset_recovery_millis(&device), 2_000);
    let runtime = device.runtime_mut();
    assert_eq!(runtime.load_states.last(), Some(&false));
    assert_eq!(runtime.clear_count, 1);
    assert!(runtime.delays.is_empty());
    assert!(runtime.events.contains(&SinkEvent::HardReset {
        direction: HardResetDirection::Received,
        cause: HardResetCause::SourceSignaled,
        recovery_ms: 2_000
    }));
    assert!(!runtime.events.contains(&SinkEvent::HardResetRecoveryComplete));
    assert!(runtime.events.iter().any(|event| matches!(
        event,
        SinkEvent::SourceCapabilities(capabilities) if capabilities.kind() == CapabilitiesKind::Spr
    )));

    DevicePolicyManager::hard_reset_recovered(&mut device);
    assert!(device.runtime_mut().events.contains(&SinkEvent::HardResetRecoveryComplete));
}

#[test]
fn identical_request_is_reported_as_a_refresh_without_interrupting_the_load() {
    let mut device = SinkDevice::new(safe_5v_config(), TestRuntime::default()).unwrap();
    let source = SourceCapabilities::new_vsafe5v_only(300);

    device.inform(&source);
    let initial = device.request(&source);
    device.transition_power(&initial);

    device.inform(&source);
    let refresh = device.request(&source);
    assert!(matches!(refresh, PowerSource::FixedVariableSupply(_)));
    assert!(device
        .runtime_mut()
        .events
        .iter()
        .any(|event| matches!(event, SinkEvent::ContractRefreshStarted(plan) if plan.object_position == 1)));
    assert_eq!(device.runtime_mut().load_states.last(), Some(&true));

    device.transition_power(&refresh);
    let runtime = device.runtime_mut();
    assert_eq!(runtime.load_states, [false, true, true]);
    assert!(runtime
        .events
        .iter()
        .any(|event| matches!(event, SinkEvent::ContractRefreshed(plan) if plan.object_position == 1)));
}

#[test]
fn same_voltage_candidate_with_sufficient_current_preserves_load_permission() {
    let mut pdos = heapless::Vec::new();
    pdos.push(PowerDataObject::FixedSupply(FixedSupply::v_safe_5v(500))).unwrap();
    pdos.push(PowerDataObject::Augmented(Augmented::Spr(
        SprProgrammablePowerSupply::default()
            .with_raw_max_current(60)
            .with_raw_min_voltage(50)
            .with_raw_max_voltage(110),
    )))
    .unwrap();
    let source = SourceCapabilities::new_with_pdos(pdos);
    let mut device = SinkDevice::new(safe_5v_config(), TestRuntime::default()).unwrap();

    device.inform(&source);
    let initial = device.request(&source);
    device.transition_power(&initial);
    assert!(matches!(block_on(device.get_event(&source)), Event::RequestSourceInfo));

    device.runtime_mut().commands.push_back(Command::Request(UserRequest::Voltage {
        voltage: Millivolts(5_000),
        current: Some(Milliamps(2_000)),
        preference: Preference::Fixed,
    }));
    let Event::RequestPower(lowered) = block_on(device.get_event(&source)) else {
        panic!("expected the 2 A fixed request")
    };
    device.transition_power(&lowered);
    assert_eq!(device.contract().confirmed_current(), Some(Milliamps(2_000)));

    let load_events_before = device.runtime_mut().load_states.len();
    device.runtime_mut().commands.push_back(Command::Request(UserRequest::Voltage {
        voltage: Millivolts(5_000),
        current: Some(Milliamps(3_000)),
        preference: Preference::Pps,
    }));
    assert!(matches!(block_on(device.get_event(&source)), Event::RequestPower(_)));

    let runtime = device.runtime_mut();
    assert_eq!(runtime.load_states.len(), load_events_before, "safe handover must not issue load-disable");
    assert!(runtime.events.iter().any(|event| matches!(
        event,
        SinkEvent::ContractTransitionStarted(transition)
            if transition.kind == ContractTransitionKind::SameVoltageSufficientCurrent
                && transition.from.is_some_and(|point| point.current == Milliamps(2_000))
                && transition.to.current == Milliamps(3_000)
    )));
}

#[test]
fn output_off_changes_only_the_application_latch() {
    let source = SourceCapabilities::new_vsafe5v_only(300);
    let mut device = SinkDevice::new(safe_5v_config(), TestRuntime::default()).unwrap();
    device.runtime_mut().commands.push_back(Command::OutputOff);
    device.runtime_mut().commands.push_back(Command::RequestSourceStatus);

    assert!(matches!(block_on(device.get_event(&source)), Event::RequestStatus));
    assert_eq!(device.contract().state(), ContractState::Detached);
    assert_eq!(device.contract().active_plan(), None);
    let runtime = device.runtime_mut();
    assert_eq!(runtime.user_output_states, [false]);
    assert!(runtime.load_states.is_empty(), "user latch must not alter PD permission");
    assert!(runtime.events.is_empty(), "output command must not masquerade as a PD transition");
}

#[test]
fn configuration_rejects_wire_truncation_and_epr_mismatch() {
    let mut config = safe_5v_config();
    config.descriptor.maximum_current = Milliamps(3_005);
    assert_eq!(config.validate(), Err(SinkConfigError::SinkCurrentResolution(Milliamps(3_005))));

    let mut config = safe_5v_config();
    config.controller = ControllerConfig {
        request_context: RequestContext {
            flags: RequestFlags { epr_capable: true, ..RequestFlags::default() },
            ..RequestContext::default()
        },
        epr_operational_pdp: Some(Milliwatts(140_000)),
    };
    config.descriptor.avs_supported = true;
    config.descriptor.epr_minimum_pdp_watts = 5;
    config.descriptor.epr_operational_pdp_watts = 139;
    config.descriptor.epr_maximum_pdp_watts = 140;
    config.max_auto_epr_attempts = 2;

    assert_eq!(
        config.validate(),
        Err(SinkConfigError::EprOperationalPdpMismatch { controller: Milliwatts(140_000), descriptor_watts: 139 })
    );

    assert!(matches!(
        SinkDevice::new_recovering(safe_5v_config(), TestRuntime::default(), recovery_intent(0)),
        Err(RecoveryInitError::AttemptsZero)
    ));
    let mut epr_intent = recovery_intent(1);
    epr_intent.mode = PortMode::Epr;
    assert!(matches!(
        SinkDevice::new_recovering(safe_5v_config(), TestRuntime::default(), epr_intent),
        Err(RecoveryInitError::EprNotSupported)
    ));
}

#[test]
fn ready_epr_source_starts_one_bounded_automatic_entry() {
    let mut config = safe_5v_config();
    config.controller = ControllerConfig {
        request_context: RequestContext {
            flags: RequestFlags { epr_capable: true, ..RequestFlags::default() },
            ..RequestContext::default()
        },
        epr_operational_pdp: Some(Milliwatts(140_000)),
    };
    config.descriptor.pps_supported = true;
    config.descriptor.avs_supported = true;
    config.descriptor.epr_minimum_pdp_watts = 5;
    config.descriptor.epr_operational_pdp_watts = 140;
    config.descriptor.epr_maximum_pdp_watts = 140;
    config.max_auto_epr_attempts = 2;

    let mut pdos = heapless::Vec::new();
    pdos.push(PowerDataObject::FixedSupply(FixedSupply::v_safe_5v(300).with_epr_mode_capable(true))).unwrap();
    let source = SourceCapabilities::new_with_pdos(pdos);
    let mut device = SinkDevice::new(config, TestRuntime::default()).unwrap();

    device.inform(&source);
    let request = device.request(&source);
    device.transition_power(&request);

    assert!(matches!(block_on(device.get_event(&source)), Event::RequestSourceInfo));
    assert!(matches!(block_on(device.get_event(&source)), Event::EnterEprMode(_)));
    assert!(device.runtime_mut().events.contains(&SinkEvent::EprDiscoveryStarted { attempt: 1, maximum_attempts: 2 }));
}

#[test]
fn alert_and_pps_status_reach_application_led_policy_without_owning_a_gpio() {
    let mut device = SinkDevice::new(safe_5v_config(), TestRuntime::default()).unwrap();
    let source = SourceCapabilities::new_vsafe5v_only(300);

    device.inform(&source);
    let request = device.request(&source);
    device.transition_power(&request);
    assert!(matches!(block_on(device.get_event(&source)), Event::RequestSourceInfo));

    let pps_status = StackPpsStatus::from_bytes(&[0x5c, 0x03, 46, 0b0000_1010]).unwrap();
    device.inform_pps_status(&pps_status);
    assert!(device
        .runtime_mut()
        .events
        .iter()
        .any(|event| matches!(event, SinkEvent::PpsStatus(status) if status.is_current_limited())));

    device.inform_alert(&AlertDataObject(1 << 28));
    assert!(matches!(block_on(device.get_event(&source)), Event::RequestStatus));
    assert!(device
        .runtime_mut()
        .events
        .iter()
        .any(|event| matches!(event, SinkEvent::SourceAlert(alert) if alert.operating_condition_changed())));
}
