use pd_sink::{
    CapabilitiesKind, ContractTransition, ContractTransitionKind, ControllerAction, ControllerConfig, ControllerError,
    Demand, EprEntryFallback, EprEntryPolicy, EprEntryRefusal, EprExitFallback, EprExitPolicy, EprExitRefusal,
    EprState, Milliamps, Millivolts, Milliwatts, PlanError, Preference, RequestContext, RequestFlags, RequestMessage,
    SinkController, SourceCapabilities, SupplyKind, UserRequest,
};

fn fixed(voltage_mv: u32, current_ma: u32, epr_capable: bool) -> u32 {
    ((voltage_mv / 50) << 10) | (current_ma / 10) | (u32::from(epr_capable) << 23)
}

fn pps(minimum_mv: u32, maximum_mv: u32, current_ma: u32) -> u32 {
    (0b11 << 30) | ((maximum_mv / 100) << 17) | ((minimum_mv / 100) << 8) | (current_ma / 50)
}

fn spr_avs(current_15v_ma: u32, current_20v_ma: u32) -> u32 {
    (0b11 << 30) | (0b10 << 28) | ((current_15v_ma / 10) << 10) | (current_20v_ma / 10)
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

fn epr_with_spr_avs(avs: u32) -> SourceCapabilities {
    SourceCapabilities::new(
        CapabilitiesKind::Epr,
        &[fixed(5_000, 3_000, true), avs, 0, 0, 0, 0, 0, fixed(48_000, 5_000, false), epr_avs(48_000, 140_000)],
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

    let action = controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None).unwrap();
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
fn targetless_epr_entry_preserves_a_confirmed_fixed_contract() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    let ControllerAction::Request(active) = controller
        .submit(UserRequest::Voltage {
            voltage: Millivolts(20_000),
            current: Some(Milliamps(2_000)),
            preference: Preference::Fixed,
        })
        .unwrap()
    else {
        panic!("expected fixed SPR request")
    };

    assert!(matches!(
        controller
            .begin_epr_discovery(EprEntryPolicy::PreserveVoltage { fallback: EprEntryFallback::Refuse }, Some(active),),
        Ok(ControllerAction::EnterEprMode { .. })
    ));
    let handover = controller.request_for_capabilities_with_contract(epr(), Some(active)).unwrap();

    assert_eq!(handover.object_position, 2);
    assert_eq!(handover.message, RequestMessage::EprRequest);
    assert_eq!(handover.encoded_voltage(), Millivolts(20_000));
    assert_eq!(handover.operating_current(), Some(Milliamps(2_000)));
    let transition = ContractTransition::classify(Some(active), handover);
    assert_eq!(transition.kind, ContractTransitionKind::SameVoltageSufficientCurrent);
    assert!(!transition.inhibits_load());
}

#[test]
fn targetless_epr_entry_preserves_a_confirmed_pps_contract() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    let ControllerAction::Request(active) = controller
        .submit(UserRequest::Voltage {
            voltage: Millivolts(19_400),
            current: Some(Milliamps(2_000)),
            preference: Preference::Pps,
        })
        .unwrap()
    else {
        panic!("expected PPS request")
    };

    controller
        .begin_epr_discovery(EprEntryPolicy::PreserveVoltage { fallback: EprEntryFallback::Refuse }, Some(active))
        .unwrap();
    let handover = controller.request_for_capabilities_with_contract(epr(), Some(active)).unwrap();

    assert_eq!(handover.object_position, 3);
    assert_eq!(handover.supply, SupplyKind::Pps);
    assert_eq!(handover.message, RequestMessage::EprRequest);
    assert_eq!(handover.encoded_voltage(), Millivolts(19_400));
    assert_eq!(handover.operating_current(), Some(Milliamps(2_000)));
    assert!(!ContractTransition::classify(Some(active), handover).inhibits_load());
}

#[test]
fn targetless_epr_entry_preserves_a_confirmed_spr_avs_contract() {
    let avs = spr_avs(4_000, 3_000);
    let spr_caps = SourceCapabilities::new(CapabilitiesKind::Spr, &[fixed(5_000, 3_000, true), avs]).unwrap();
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr_caps).unwrap();
    let ControllerAction::Request(active) = controller
        .submit(UserRequest::Voltage {
            voltage: Millivolts(19_400),
            current: Some(Milliamps(2_000)),
            preference: Preference::SprAvs,
        })
        .unwrap()
    else {
        panic!("expected SPR AVS request")
    };

    controller
        .begin_epr_discovery(EprEntryPolicy::PreserveVoltage { fallback: EprEntryFallback::Refuse }, Some(active))
        .unwrap();
    let handover = controller.request_for_capabilities_with_contract(epr_with_spr_avs(avs), Some(active)).unwrap();

    assert_eq!(handover.object_position, 2);
    assert_eq!(handover.supply, SupplyKind::SprAvs);
    assert_eq!(handover.message, RequestMessage::EprRequest);
    assert_eq!(handover.encoded_voltage(), Millivolts(19_400));
    assert_eq!(handover.operating_current(), Some(Milliamps(2_000)));
    assert!(!ContractTransition::classify(Some(active), handover).inhibits_load());
}

#[test]
fn preserve_entry_refuses_before_entry_without_a_confirmed_contract() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();

    assert_eq!(
        controller.begin_epr_discovery(EprEntryPolicy::PreserveVoltage { fallback: EprEntryFallback::Refuse }, None,),
        Err(ControllerError::EprEntryRefused(EprEntryRefusal::NoConfirmedContract))
    );
    assert_eq!(controller.epr_state(), EprState::Spr);
}

#[test]
fn changed_epr_capabilities_apply_the_explicit_safe_five_volt_entry_fallback() {
    let changed = SourceCapabilities::new(
        CapabilitiesKind::Epr,
        &[
            fixed(5_000, 3_000, true),
            fixed(20_000, 1_000, false),
            pps(5_000, 21_000, 1_000),
            0,
            0,
            0,
            0,
            fixed(48_000, 5_000, false),
            epr_avs(48_000, 140_000),
        ],
    )
    .unwrap();
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    let ControllerAction::Request(active) = controller
        .submit(UserRequest::Voltage {
            voltage: Millivolts(20_000),
            current: Some(Milliamps(2_000)),
            preference: Preference::Fixed,
        })
        .unwrap()
    else {
        panic!("expected fixed SPR request")
    };

    controller
        .begin_epr_discovery(EprEntryPolicy::PreserveVoltage { fallback: EprEntryFallback::Safe5V }, Some(active))
        .unwrap();
    let fallback = controller.request_for_capabilities_with_contract(changed, Some(active)).unwrap();

    assert_eq!(fallback.object_position, 1);
    assert_eq!(fallback.supply, SupplyKind::Fixed);
    assert_eq!(fallback.encoded_voltage(), Millivolts(5_000));
    assert_eq!(controller.epr_state(), EprState::Epr);
    assert!(ContractTransition::classify(Some(active), fallback).inhibits_load());
    assert_eq!(controller.take_pending_error(), None);
}

#[test]
fn changed_epr_capabilities_make_refuse_policy_return_to_spr_without_claiming_continuity() {
    let changed = SourceCapabilities::new(
        CapabilitiesKind::Epr,
        &[
            fixed(5_000, 3_000, true),
            fixed(20_000, 1_000, false),
            pps(5_000, 21_000, 1_000),
            0,
            0,
            0,
            0,
            fixed(48_000, 5_000, false),
            epr_avs(48_000, 140_000),
        ],
    )
    .unwrap();
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    let ControllerAction::Request(active) = controller
        .submit(UserRequest::Voltage {
            voltage: Millivolts(20_000),
            current: Some(Milliamps(2_000)),
            preference: Preference::Fixed,
        })
        .unwrap()
    else {
        panic!("expected fixed SPR request")
    };

    controller
        .begin_epr_discovery(EprEntryPolicy::PreserveVoltage { fallback: EprEntryFallback::Refuse }, Some(active))
        .unwrap();
    let recovery = controller.request_for_capabilities_with_contract(changed, Some(active)).unwrap();

    assert_eq!(recovery.encoded_voltage(), Millivolts(5_000));
    assert_eq!(controller.epr_state(), EprState::Exiting);
    assert_eq!(
        controller.take_pending_error(),
        Some(ControllerError::EprEntryRefused(EprEntryRefusal::CapabilitiesChanged))
    );
    assert!(ContractTransition::classify(Some(active), recovery).inhibits_load());
    controller.on_ps_ready();
    assert_eq!(controller.take_ready_action(), Some(ControllerAction::ExitEprMode));
}

#[test]
fn power_limited_unknown_epr_current_uses_the_explicit_entry_fallback() {
    let power_limited_epr = SourceCapabilities::new(
        CapabilitiesKind::Epr,
        &[
            fixed(5_000, 3_000, true),
            0,
            pps(5_000, 21_000, 3_000) | (1 << 27),
            0,
            0,
            0,
            0,
            fixed(48_000, 5_000, false),
            epr_avs(48_000, 140_000),
        ],
    )
    .unwrap();
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    let ControllerAction::Request(active) = controller
        .submit(UserRequest::Voltage {
            voltage: Millivolts(19_400),
            current: Some(Milliamps(2_000)),
            preference: Preference::Pps,
        })
        .unwrap()
    else {
        panic!("expected PPS request")
    };

    controller
        .begin_epr_discovery(EprEntryPolicy::PreserveVoltage { fallback: EprEntryFallback::Safe5V }, Some(active))
        .unwrap();
    let fallback = controller.request_for_capabilities_with_contract(power_limited_epr, Some(active)).unwrap();

    assert_eq!(fallback.encoded_voltage(), Millivolts(5_000));
    assert!(ContractTransition::classify(Some(active), fallback).inhibits_load());
}

#[test]
fn epr_discovery_requires_an_epr_capable_source() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(false)).unwrap();

    assert_eq!(controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None), Err(ControllerError::EprUnavailable));
    assert_eq!(controller.epr_state(), EprState::Spr);
}

#[test]
fn fresh_spr_caps_rearm_discovery_after_interrupted_epr_entry() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    assert!(matches!(
        controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None),
        Ok(ControllerAction::EnterEprMode { .. })
    ));
    assert_eq!(controller.epr_state(), EprState::Entering);

    // A Soft Reset during entry is followed by ordinary SPR capabilities.
    // Observing them must synchronize the DPM back to SPR and permit a retry.
    controller.request_for_capabilities(spr(true)).unwrap();
    assert_eq!(controller.epr_state(), EprState::Spr);
    assert!(matches!(
        controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None),
        Ok(ControllerAction::EnterEprMode { .. })
    ));
}

#[test]
fn temporary_five_volt_caps_do_not_prevent_later_epr_discovery() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(false)).unwrap();
    assert_eq!(controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None), Err(ControllerError::EprUnavailable));

    controller.request_for_capabilities(spr(true)).unwrap();
    assert!(matches!(
        controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None),
        Ok(ControllerAction::EnterEprMode { .. })
    ));
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
    controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None).unwrap();
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
    let active = controller.request_for_capabilities(epr()).unwrap();

    let ControllerAction::Request(plan) = controller.exit_epr(EprExitPolicy::Safe5V, Some(active)).unwrap() else {
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
    let active = controller.request_for_capabilities(epr()).unwrap();
    controller.exit_epr(EprExitPolicy::Safe5V, Some(active)).unwrap();

    controller.request_rejected();

    assert_eq!(controller.epr_state(), EprState::Epr);
    assert_eq!(controller.take_ready_action(), None);
    assert!(matches!(controller.exit_epr(EprExitPolicy::Safe5V, Some(active)), Ok(ControllerAction::Request(_))));
}

#[test]
fn deferred_pre_exit_request_remains_armed_until_the_retry_reaches_ps_rdy() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller
        .submit(UserRequest::Voltage { voltage: Millivolts(48_000), current: None, preference: Preference::Fixed })
        .unwrap();
    let active = controller.request_for_capabilities(epr()).unwrap();
    controller.exit_epr(EprExitPolicy::Safe5V, Some(active)).unwrap();

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
fn epr_exit_is_direct_when_the_confirmed_contract_already_uses_an_spr_object() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None).unwrap();
    controller.request_for_capabilities(epr()).unwrap();
    let ControllerAction::Request(active) =
        controller.submit(UserRequest::Pdo { position: 2, demand: Demand::Current(Milliamps(2_000)) }).unwrap()
    else {
        panic!("expected EPR_Request using SPR object 2")
    };

    assert_eq!(active.message, RequestMessage::EprRequest);
    assert_eq!(active.object_position, 2);
    assert_eq!(
        controller
            .exit_epr(EprExitPolicy::PreserveVoltage { fallback: EprExitFallback::Refuse }, Some(active))
            .unwrap(),
        ControllerAction::ExitEprMode
    );
    assert_eq!(controller.epr_state(), EprState::Exiting);
}

#[test]
fn safe_five_volt_exit_is_direct_when_fixed_five_volts_is_already_confirmed() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None).unwrap();
    let active = controller.request_for_capabilities(epr()).unwrap();
    assert_eq!(active.object_position, 1);
    assert_eq!(active.message, RequestMessage::EprRequest);

    assert_eq!(controller.exit_epr(EprExitPolicy::Safe5V, Some(active)).unwrap(), ControllerAction::ExitEprMode);
}

#[test]
fn preserve_voltage_hands_an_epr_only_contract_to_a_sufficient_spr_object_before_exit() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None).unwrap();
    controller.request_for_capabilities(epr()).unwrap();
    let ControllerAction::Request(active) = controller
        .submit(UserRequest::Voltage {
            voltage: Millivolts(20_000),
            current: Some(Milliamps(2_000)),
            preference: Preference::EprAvs,
        })
        .unwrap()
    else {
        panic!("expected EPR-only AVS request")
    };
    assert_eq!(active.object_position, 9);

    let ControllerAction::Request(handover) = controller
        .exit_epr(EprExitPolicy::PreserveVoltage { fallback: EprExitFallback::Refuse }, Some(active))
        .unwrap()
    else {
        panic!("expected same-voltage SPR handover")
    };
    assert_eq!(handover.object_position, 2);
    assert_eq!(handover.message, RequestMessage::EprRequest);
    assert_eq!(handover.encoded_voltage(), Millivolts(20_000));
    assert_eq!(handover.operating_current(), Some(Milliamps(2_000)));
    let transition = ContractTransition::classify(Some(active), handover);
    assert_eq!(transition.kind, ContractTransitionKind::SameVoltageSufficientCurrent);
    assert!(!transition.inhibits_load(), "same-voltage handover must preserve external load permission");
    assert_eq!(controller.take_ready_action(), None);
    controller.on_ps_ready();
    assert_eq!(controller.take_ready_action(), Some(ControllerAction::ExitEprMode));
}

#[test]
fn preserve_voltage_refuses_insufficient_spr_current_without_changing_epr_state() {
    let limited = SourceCapabilities::new(
        CapabilitiesKind::Epr,
        &[
            fixed(5_000, 3_000, true),
            fixed(20_000, 1_000, false),
            pps(5_000, 21_000, 1_000),
            0,
            0,
            0,
            0,
            fixed(48_000, 5_000, false),
            epr_avs(48_000, 140_000),
        ],
    )
    .unwrap();
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None).unwrap();
    controller.request_for_capabilities(limited).unwrap();
    let ControllerAction::Request(active) = controller
        .submit(UserRequest::Voltage {
            voltage: Millivolts(20_000),
            current: Some(Milliamps(2_000)),
            preference: Preference::EprAvs,
        })
        .unwrap()
    else {
        panic!("expected EPR-only AVS request")
    };
    let desired = controller.desired();

    assert_eq!(
        controller.exit_epr(EprExitPolicy::PreserveVoltage { fallback: EprExitFallback::Refuse }, Some(active)),
        Err(ControllerError::EprExitRefused(EprExitRefusal::NoSuitableSprContract))
    );
    assert_eq!(controller.epr_state(), EprState::Epr);
    assert_eq!(controller.desired(), desired);
}

#[test]
fn preserve_voltage_treats_power_limited_unknown_current_as_unsafe() {
    let uncertain = SourceCapabilities::new(
        CapabilitiesKind::Epr,
        &[
            fixed(5_000, 3_000, true),
            pps(5_000, 21_000, 3_000) | (1 << 27),
            0,
            0,
            0,
            0,
            0,
            fixed(48_000, 5_000, false),
            epr_avs(48_000, 140_000),
        ],
    )
    .unwrap();
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None).unwrap();
    controller.request_for_capabilities(uncertain).unwrap();
    let ControllerAction::Request(active) = controller
        .submit(UserRequest::Voltage {
            voltage: Millivolts(20_000),
            current: Some(Milliamps(2_000)),
            preference: Preference::EprAvs,
        })
        .unwrap()
    else {
        panic!("expected EPR-only AVS request")
    };

    assert_eq!(
        controller.exit_epr(EprExitPolicy::PreserveVoltage { fallback: EprExitFallback::Refuse }, Some(active)),
        Err(ControllerError::EprExitRefused(EprExitRefusal::NoSuitableSprContract))
    );
}

#[test]
fn changed_capabilities_cancel_a_refuse_policy_exit_without_sending_epr_exit() {
    let changed = SourceCapabilities::new(
        CapabilitiesKind::Epr,
        &[
            fixed(5_000, 3_000, true),
            fixed(20_000, 1_000, false),
            pps(5_000, 21_000, 1_000),
            0,
            0,
            0,
            0,
            fixed(48_000, 5_000, false),
            epr_avs(48_000, 140_000),
        ],
    )
    .unwrap();
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None).unwrap();
    controller.request_for_capabilities(epr()).unwrap();
    let ControllerAction::Request(active) = controller
        .submit(UserRequest::Voltage {
            voltage: Millivolts(20_000),
            current: Some(Milliamps(2_000)),
            preference: Preference::EprAvs,
        })
        .unwrap()
    else {
        panic!("expected EPR-only AVS request")
    };
    assert!(matches!(
        controller.exit_epr(EprExitPolicy::PreserveVoltage { fallback: EprExitFallback::Refuse }, Some(active)),
        Ok(ControllerAction::Request(_))
    ));
    controller.request_deferred();

    let recovery = controller.request_for_capabilities_with_contract(changed, Some(active)).unwrap();
    assert_eq!(recovery.encoded_voltage(), Millivolts(5_000));
    assert_eq!(controller.epr_state(), EprState::Epr);
    assert_eq!(
        controller.take_pending_error(),
        Some(ControllerError::EprExitRefused(EprExitRefusal::CapabilitiesChanged))
    );
    controller.on_ps_ready();
    assert_eq!(controller.take_ready_action(), None, "refusal recovery must remain in EPR mode");
}

#[test]
fn preserve_voltage_safe_fallback_establishes_fixed_five_volts_before_exit() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller
        .submit(UserRequest::Voltage { voltage: Millivolts(48_000), current: None, preference: Preference::Fixed })
        .unwrap();
    let active = controller.request_for_capabilities(epr()).unwrap();

    let ControllerAction::Request(fallback) = controller
        .exit_epr(EprExitPolicy::PreserveVoltage { fallback: EprExitFallback::Safe5V }, Some(active))
        .unwrap()
    else {
        panic!("expected fixed 5 V fallback")
    };
    assert_eq!(fallback.object_position, 1);
    assert_eq!(fallback.supply, SupplyKind::Fixed);
    assert_eq!(fallback.encoded_voltage(), Millivolts(5_000));
    assert_eq!(controller.take_ready_action(), None);
}

#[test]
fn preserve_voltage_requires_a_confirmed_contract() {
    let mut controller = configured_controller();
    controller.request_for_capabilities(spr(true)).unwrap();
    controller.begin_epr_discovery(EprEntryPolicy::Safe5V, None).unwrap();
    controller.request_for_capabilities(epr()).unwrap();

    assert_eq!(
        controller.exit_epr(EprExitPolicy::PreserveVoltage { fallback: EprExitFallback::Refuse }, None),
        Err(ControllerError::EprExitRefused(EprExitRefusal::NoConfirmedContract))
    );
    assert_eq!(controller.epr_state(), EprState::Epr);
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
