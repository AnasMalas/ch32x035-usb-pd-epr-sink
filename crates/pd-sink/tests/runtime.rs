use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};

use pd_sink::{
    CapabilitiesKind, Command, ContractState, ControllerConfig, HardResetDirection, Milliamps, Milliwatts,
    RequestContext, RequestFlags, SinkConfig, SinkConfigError, SinkDevice, SinkEvent, SinkPowerDescriptor, SinkRuntime,
};
use usbpd::protocol_layer::message::data::alert::AlertDataObject;
use usbpd::protocol_layer::message::data::request::PowerSource;
use usbpd::protocol_layer::message::data::source_capabilities::{FixedSupply, PowerDataObject, SourceCapabilities};
use usbpd::protocol_layer::message::extended::pps_status::PpsStatus as StackPpsStatus;
use usbpd::sink::device_policy_manager::{DevicePolicyManager, Event, HardResetOrigin};

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
    clear_count: usize,
    delays: Vec<u64>,
}

impl SinkRuntime for TestRuntime {
    fn set_load_enabled(&mut self, enabled: bool) {
        self.load_states.push(enabled);
    }

    fn clear_pending_commands(&mut self) {
        self.clear_count += 1;
    }

    fn observe(&mut self, event: SinkEvent) {
        self.events.push(event);
    }

    async fn wait_for_command(&mut self) -> Command {
        panic!("this test does not drive the command future")
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

#[test]
fn reusable_device_owns_contract_and_hard_reset_safety() {
    let mut device = SinkDevice::new(safe_5v_config(), TestRuntime::default()).unwrap();
    let source = SourceCapabilities::new_vsafe5v_only(300);

    block_on(device.inform(&source));
    let request = block_on(device.request(&source));
    assert!(matches!(request, PowerSource::FixedVariableSupply(_)));
    assert_eq!(device.contract().state(), ContractState::Pending);

    block_on(device.transition_power(&request));
    assert_eq!(device.contract().state(), ContractState::Ready);
    assert_eq!(device.contract().active_plan().unwrap().object_position, 1);
    assert_eq!(device.runtime_mut().load_states.last(), Some(&true));

    block_on(device.hard_reset(HardResetOrigin::Source));
    assert_eq!(device.contract().state(), ContractState::Lost);
    let runtime = device.runtime_mut();
    assert_eq!(runtime.load_states.last(), Some(&false));
    assert_eq!(runtime.clear_count, 1);
    assert_eq!(runtime.delays, [2_000]);
    assert!(runtime
        .events
        .contains(&SinkEvent::HardReset { direction: HardResetDirection::Received, recovery_ms: 2_000 }));
    assert!(runtime.events.contains(&SinkEvent::HardResetRecoveryComplete));
    assert!(runtime.events.iter().any(|event| matches!(
        event,
        SinkEvent::SourceCapabilities(capabilities) if capabilities.kind() == CapabilitiesKind::Spr
    )));
}

#[test]
fn identical_request_is_reported_as_a_refresh_without_interrupting_the_load() {
    let mut device = SinkDevice::new(safe_5v_config(), TestRuntime::default()).unwrap();
    let source = SourceCapabilities::new_vsafe5v_only(300);

    block_on(device.inform(&source));
    let initial = block_on(device.request(&source));
    block_on(device.transition_power(&initial));

    block_on(device.inform(&source));
    let refresh = block_on(device.request(&source));
    assert!(matches!(refresh, PowerSource::FixedVariableSupply(_)));
    assert!(device
        .runtime_mut()
        .events
        .iter()
        .any(|event| matches!(event, SinkEvent::ContractRefreshStarted(plan) if plan.object_position == 1)));
    assert_eq!(device.runtime_mut().load_states.last(), Some(&true));

    block_on(device.transition_power(&refresh));
    let runtime = device.runtime_mut();
    assert_eq!(runtime.load_states, [false, true, true]);
    assert!(runtime
        .events
        .iter()
        .any(|event| matches!(event, SinkEvent::ContractRefreshed(plan) if plan.object_position == 1)));
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

    block_on(device.inform(&source));
    let request = block_on(device.request(&source));
    block_on(device.transition_power(&request));

    assert!(matches!(block_on(device.get_event(&source)), Event::RequestSourceInfo));
    assert!(matches!(block_on(device.get_event(&source)), Event::EnterEprMode(_)));
    assert!(device.runtime_mut().events.contains(&SinkEvent::EprDiscoveryStarted { attempt: 1, maximum_attempts: 2 }));
}

#[test]
fn alert_and_pps_status_reach_application_led_policy_without_owning_a_gpio() {
    let mut device = SinkDevice::new(safe_5v_config(), TestRuntime::default()).unwrap();
    let source = SourceCapabilities::new_vsafe5v_only(300);

    block_on(device.inform(&source));
    let request = block_on(device.request(&source));
    block_on(device.transition_power(&request));
    assert!(matches!(block_on(device.get_event(&source)), Event::RequestSourceInfo));

    let pps_status = StackPpsStatus::from_bytes(&[0x5c, 0x03, 46, 0b0000_1010]).unwrap();
    block_on(device.inform_pps_status(&pps_status));
    assert!(device
        .runtime_mut()
        .events
        .iter()
        .any(|event| matches!(event, SinkEvent::PpsStatus(status) if status.is_current_limited())));

    block_on(device.inform_alert(&AlertDataObject(1 << 28)));
    assert!(matches!(block_on(device.get_event(&source)), Event::RequestStatus));
    assert!(device
        .runtime_mut()
        .events
        .iter()
        .any(|event| matches!(event, SinkEvent::SourceAlert(alert) if alert.operating_condition_changed())));
}
