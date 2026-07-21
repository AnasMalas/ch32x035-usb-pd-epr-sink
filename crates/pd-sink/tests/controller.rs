use pd_sink::{
    CapabilitiesKind, ControllerAction, ControllerConfig, ControllerError, Demand, EprState, Milliamps, Millivolts,
    Milliwatts, PlanError, Preference, RequestContext, RequestFlags, RequestMessage, SinkController,
    SourceCapabilities, SupplyKind, UserRequest,
};

fn fixed(voltage_mv: u32, current_ma: u32, epr_capable: bool) -> u32 {
    ((voltage_mv / 50) << 10) | (current_ma / 10) | (u32::from(epr_capable) << 23)
}

fn pps(minimum_mv: u32, maximum_mv: u32, current_ma: u32) -> u32 {
    (0b11 << 30) | ((maximum_mv / 100) << 17) | ((minimum_mv / 100) << 8) | (current_ma / 50)
}

fn epr_avs(maximum_mv: u32, pdp_mw: u32) -> u32 {
    epr_avs_range(15_000, maximum_mv, pdp_mw)
}

fn epr_avs_range(minimum_mv: u32, maximum_mv: u32, pdp_mw: u32) -> u32 {
    (0b11 << 30) | (0b01 << 28) | ((maximum_mv / 100) << 17) | ((minimum_mv / 100) << 8) | (pdp_mw / 1_000)
}

fn spr(epr_capable: bool) -> SourceCapabilities {
    SourceCapabilities::new(
        CapabilitiesKind::Spr,
        &[fixed(5_000, 3_000, epr_capable), fixed(20_000, 5_000, false), pps(5_000, 21_000, 3_000)],
    )
    .unwrap()
}

fn epr() -> SourceCapabilities {
    epr_with_avs(epr_avs(48_000, 140_000))
}

fn epr_with_avs(avs: u32) -> SourceCapabilities {
    SourceCapabilities::new(
        CapabilitiesKind::Epr,
        &[
            fixed(5_000, 3_000, true),
            fixed(20_000, 5_000, false),
            pps(5_000, 21_000, 3_000),
            0,
            0,
            0,
            0,
            fixed(48_000, 5_000, false),
            avs,
        ],
    )
    .unwrap()
}

fn configured_controller() -> SinkController {
    SinkController::new(ControllerConfig {
        request_context: RequestContext {
            flags: RequestFlags { epr_capable: true, ..RequestFlags::default() },
            ..RequestContext::default()
        },
        epr_operational_pdp: Some(Milliwatts(140_000)),
    })
}

#[test]
fn a_new_attachment_starts_with_a_safe_five_volt_request() {
    let mut controller = configured_controller();
    let plan = controller.request_for_capabilities(spr(true)).unwrap();

    assert_eq!(plan.object_position, 1);
    assert_eq!(plan.message, RequestMessage::Request);
    assert_eq!(plan.operating_current(), Some(Milliamps(3_000)));
    assert_ne!(plan.rdo & (1 << 22), 0, "the SPR contract must declare EPR capability before entry");
    assert_eq!(controller.epr_state(), EprState::Spr);
}

#[test]
fn epr_discovery_retains_no_high_voltage_intent_and_stays_at_five_volts() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();

    let action = controller.begin_epr_discovery().unwrap();
    assert_eq!(action, ControllerAction::EnterEprMode { operational_pdp: Milliwatts(140_000) });
    assert_eq!(controller.epr_state(), EprState::Entering);
    assert_eq!(controller.desired(), None);

    let plan = controller.request_for_capabilities(epr()).unwrap();
    assert_eq!(controller.epr_state(), EprState::Epr);
    assert_eq!(plan.object_position, 1);
    assert_eq!(plan.supply, SupplyKind::Fixed);
    assert_eq!(plan.message, RequestMessage::EprRequest);
}

#[test]
fn epr_discovery_requires_an_epr_capable_source() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(false)).unwrap();

    assert_eq!(controller.begin_epr_discovery(), Err(ControllerError::EprUnavailable));
    assert_eq!(controller.epr_state(), EprState::Spr);
}

#[test]
fn fresh_spr_caps_rearm_discovery_after_interrupted_epr_entry() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    assert!(matches!(controller.begin_epr_discovery(), Ok(ControllerAction::EnterEprMode { .. })));
    assert_eq!(controller.epr_state(), EprState::Entering);

    // A Soft Reset during entry is followed by ordinary SPR capabilities.
    // Observing them must synchronize the DPM back to SPR and permit a retry.
    controller.request_for_capabilities(spr(true)).unwrap();
    assert_eq!(controller.epr_state(), EprState::Spr);
    assert!(matches!(controller.begin_epr_discovery(), Ok(ControllerAction::EnterEprMode { .. })));
}

#[test]
fn temporary_five_volt_caps_do_not_prevent_later_epr_discovery() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(false)).unwrap();
    assert_eq!(controller.begin_epr_discovery(), Err(ControllerError::EprUnavailable));

    controller.request_for_capabilities(spr(true)).unwrap();
    assert!(matches!(controller.begin_epr_discovery(), Ok(ControllerAction::EnterEprMode { .. })));
}

#[test]
fn pps_request_is_immediate_when_spr_offer_can_satisfy_it() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();

    let action = controller
        .submit(UserRequest::Voltage { voltage: Millivolts(19_400), current: None, preference: Preference::Pps })
        .unwrap();

    let ControllerAction::Request(plan) = action else { panic!("expected request") };
    assert_eq!(plan.supply, SupplyKind::Pps);
    assert_eq!(plan.message, RequestMessage::Request);
    assert_eq!((plan.rdo >> 9) & 0xfff, 970);
}

#[test]
fn forty_eight_volts_drives_epr_entry_then_epr_request() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();

    let action = controller
        .submit(UserRequest::Voltage {
            voltage: Millivolts(48_000),
            current: Some(Milliamps(2_000)),
            preference: Preference::Fixed,
        })
        .unwrap();
    assert_eq!(action, ControllerAction::EnterEprMode { operational_pdp: Milliwatts(140_000) });
    assert_eq!(controller.epr_state(), EprState::Entering);

    let plan = controller.request_for_capabilities(epr()).unwrap();
    assert_eq!(controller.epr_state(), EprState::Epr);
    assert_eq!(plan.object_position, 8);
    assert_eq!(plan.message, RequestMessage::EprRequest);
    assert_eq!(plan.operating_current(), Some(Milliamps(2_000)));
}

#[test]
fn epr_avs_can_supply_nineteen_point_four_when_spr_cannot() {
    let spr_without_adjustable = SourceCapabilities::new(CapabilitiesKind::Spr, &[fixed(5_000, 3_000, true)]).unwrap();
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr_without_adjustable).unwrap();

    let action = controller
        .submit(UserRequest::Voltage { voltage: Millivolts(19_400), current: None, preference: Preference::EprAvs })
        .unwrap();
    assert!(matches!(action, ControllerAction::EnterEprMode { .. }));

    let plan = controller.request_for_capabilities(epr()).unwrap();
    assert_eq!(plan.object_position, 9);
    assert_eq!(plan.supply, SupplyKind::EprAvs);
    assert_eq!((plan.rdo >> 9) & 0xfff, 776);
}

#[test]
fn nonstandard_epr_avs_requires_an_explicit_preference_before_epr_entry() {
    let spr_without_adjustable = SourceCapabilities::new(CapabilitiesKind::Spr, &[fixed(5_000, 3_000, true)]).unwrap();
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr_without_adjustable).unwrap();

    assert_eq!(
        controller.submit(UserRequest::Voltage {
            voltage: Millivolts(10_000),
            current: None,
            preference: Preference::EprAvs,
        }),
        Err(ControllerError::Plan(PlanError::VoltageUnavailable(Millivolts(10_000))))
    );
    assert_eq!(controller.epr_state(), EprState::Spr);

    let action = controller
        .submit(UserRequest::Voltage {
            voltage: Millivolts(10_000),
            current: None,
            preference: Preference::EprAvsNonstandard,
        })
        .unwrap();
    assert!(matches!(action, ControllerAction::EnterEprMode { .. }));

    let plan = controller.request_for_capabilities(epr_with_avs(epr_avs_range(5_000, 28_000, 140_000))).unwrap();
    assert_eq!(plan.object_position, 9);
    assert_eq!(plan.supply, SupplyKind::EprAvs);
    assert_eq!((plan.rdo >> 9) & 0xfff, 400);
}

#[test]
fn direct_epr_pdo_selection_is_retained_across_entry() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();

    let action = controller.submit(UserRequest::Pdo { position: 8, demand: Demand::Maximum }).unwrap();
    assert!(matches!(action, ControllerAction::EnterEprMode { .. }));

    let plan = controller.request_for_capabilities(epr()).unwrap();
    assert_eq!(plan.object_position, 8);
    assert_eq!(plan.message, RequestMessage::EprRequest);
}

#[test]
fn direct_epr_avs_maximum_is_retained_across_entry() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();

    let action = controller.submit(UserRequest::Pdo { position: 9, demand: Demand::Maximum }).unwrap();
    assert!(matches!(action, ControllerAction::EnterEprMode { .. }));

    let plan = controller.request_for_capabilities(epr()).unwrap();
    assert_eq!(plan.object_position, 9);
    assert_eq!(plan.supply, SupplyKind::EprAvs);
    assert_eq!(plan.message, RequestMessage::EprRequest);
    assert_eq!(
        plan.voltage,
        pd_sink::PlannedVoltage::Adjustable {
            requested: Millivolts(48_000),
            encoded: Millivolts(48_000),
            step_mv: 100,
        }
    );
}

#[test]
fn previewing_every_offer_does_not_change_epr_state_or_user_intent() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller.begin_epr_discovery().unwrap();
    controller.request_for_capabilities(epr()).unwrap();

    assert_eq!(controller.epr_state(), EprState::Epr);
    assert_eq!(controller.desired(), None);
    for position in [1, 2, 3, 8, 9] {
        let plan = controller.preview(UserRequest::Pdo { position, demand: Demand::Maximum }).unwrap();
        assert_eq!(plan.object_position, position);
    }
    assert_eq!(controller.epr_state(), EprState::Epr);
    assert_eq!(controller.desired(), None);

    assert!(controller.preview(UserRequest::Pdo { position: 11, demand: Demand::Maximum }).is_err());
    assert_eq!(controller.epr_state(), EprState::Epr);
    assert_eq!(controller.desired(), None);
}

#[test]
fn epr_requires_both_source_support_and_explicit_sink_pdp() {
    let request = UserRequest::Voltage { voltage: Millivolts(48_000), current: None, preference: Preference::Fixed };

    let mut unsupported = configured_controller();
    unsupported.request_for_capabilities(spr(false)).unwrap();
    assert!(matches!(unsupported.submit(request), Err(ControllerError::Plan(_))));

    let mut unconfigured = SinkController::new(ControllerConfig::default());
    unconfigured.request_for_capabilities(spr(true)).unwrap();
    assert_eq!(unconfigured.submit(request), Err(ControllerError::EprNotConfigured));
}

#[test]
fn detach_forgets_high_voltage_intent_and_returns_to_five_volts() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller
        .submit(UserRequest::Voltage { voltage: Millivolts(19_400), current: None, preference: Preference::Pps })
        .unwrap();

    controller.reset_port();
    assert_eq!(controller.desired(), None);
    assert_eq!(controller.epr_state(), EprState::Spr);

    let plan = controller.request_for_capabilities(spr(true)).unwrap();
    assert_eq!(plan.object_position, 1);
}

#[test]
fn changed_capabilities_clear_an_unavailable_request_and_fall_back_to_five_volts() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller
        .submit(UserRequest::Voltage { voltage: Millivolts(19_400), current: None, preference: Preference::Pps })
        .unwrap();

    let five_volts_only = SourceCapabilities::new(CapabilitiesKind::Spr, &[fixed(5_000, 3_000, true)]).unwrap();
    let plan = controller.request_for_capabilities(five_volts_only).unwrap();

    assert_eq!(controller.desired(), None);
    assert_eq!(plan.object_position, 1);
    assert_eq!(plan.supply, SupplyKind::Fixed);
    assert_eq!(plan.message, RequestMessage::Request);
}

#[test]
fn epr_exit_first_establishes_a_five_volt_spr_contract() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller
        .submit(UserRequest::Voltage { voltage: Millivolts(48_000), current: None, preference: Preference::Fixed })
        .unwrap();
    controller.request_for_capabilities(epr()).unwrap();

    let ControllerAction::Request(plan) = controller.exit_epr().unwrap() else {
        panic!("expected the pre-exit SPR request")
    };
    assert_eq!(controller.epr_state(), EprState::Exiting);
    assert_eq!(plan.object_position, 1);
    assert_eq!(plan.message, RequestMessage::EprRequest);
    assert_eq!(plan.supply, SupplyKind::Fixed);
    assert_eq!(controller.take_ready_action(), None);

    controller.on_ps_ready();
    assert_eq!(controller.take_ready_action(), Some(ControllerAction::ExitEprMode));
    assert_eq!(controller.take_ready_action(), None);

    controller.observe_capabilities(spr(true));
    assert_eq!(controller.epr_state(), EprState::Spr);
}

#[test]
fn rejected_pre_exit_request_keeps_the_controller_in_epr() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller
        .submit(UserRequest::Voltage { voltage: Millivolts(48_000), current: None, preference: Preference::Fixed })
        .unwrap();
    controller.request_for_capabilities(epr()).unwrap();
    controller.exit_epr().unwrap();

    controller.request_rejected();

    assert_eq!(controller.epr_state(), EprState::Epr);
    assert_eq!(controller.take_ready_action(), None);
    assert!(matches!(controller.exit_epr(), Ok(ControllerAction::Request(_))));
}

#[test]
fn deferred_pre_exit_request_remains_armed_until_the_retry_reaches_ps_rdy() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller
        .submit(UserRequest::Voltage { voltage: Millivolts(48_000), current: None, preference: Preference::Fixed })
        .unwrap();
    controller.request_for_capabilities(epr()).unwrap();
    controller.exit_epr().unwrap();

    controller.request_deferred();
    assert_eq!(controller.epr_state(), EprState::Exiting);
    assert_eq!(controller.take_ready_action(), None);

    let retry = controller.request_for_capabilities(epr()).unwrap();
    assert_eq!(retry.object_position, 1);
    assert_eq!(retry.message, RequestMessage::EprRequest);
    assert_eq!(controller.epr_state(), EprState::Exiting);

    controller.on_ps_ready();
    assert_eq!(controller.take_ready_action(), Some(ControllerAction::ExitEprMode));
}

#[test]
fn source_info_refines_power_limited_pps_current() {
    let power_limited_pps = pps(5_000, 21_000, 5_000) | (1 << 27);
    let capabilities =
        SourceCapabilities::new(CapabilitiesKind::Spr, &[fixed(5_000, 3_000, false), power_limited_pps]).unwrap();
    let mut controller = configured_controller();
    controller.request_for_capabilities(capabilities).unwrap();
    controller.set_source_present_pdp(Some(Milliwatts(65_000)));

    let ControllerAction::Request(plan) = controller
        .submit(UserRequest::Voltage { voltage: Millivolts(19_400), current: None, preference: Preference::Pps })
        .unwrap()
    else {
        panic!("expected PPS request")
    };

    assert_eq!(plan.operating_current(), Some(Milliamps(3_350)));

    controller.reset_port();
    controller.request_for_capabilities(capabilities).unwrap();
    let ControllerAction::Request(plan) = controller
        .submit(UserRequest::Voltage { voltage: Millivolts(19_400), current: None, preference: Preference::Pps })
        .unwrap()
    else {
        panic!("expected PPS request")
    };
    assert_eq!(plan.operating_current(), Some(Milliamps(5_000)));
}
