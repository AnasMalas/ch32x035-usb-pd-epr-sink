use pd_sink::capabilities::CapabilityListError;
use pd_sink::request::PlanError;
use pd_sink::{
    capabilities_from_stack, request_to_stack, CapabilitiesKind, ContractState, ContractTracker,
    ContractTransitionKind, CurrentConfidence, Demand, LimitReason, Milliamps, Millivolts, Milliwatts, PdoError,
    PdoValidity, PlannedOperating, PlannedVoltage, PortInputs, PortMode, PortState, PortSupervisor, Preference,
    RequestContext, RequestMessage, RequestPlanner, SafetyTimings, SinkLimits, SourceCapabilities, SourceSupply,
    SupplyKind,
};
use usbpd::protocol_layer::message::data::request::PowerSource;
use usbpd::protocol_layer::message::data::source_capabilities::SourceCapabilities as StackSourceCapabilities;

fn fixed(voltage_mv: u32, current_ma: u32, epr_capable: bool) -> u32 {
    ((voltage_mv / 50) << 10) | (current_ma / 10) | (u32::from(epr_capable) << 23)
}

fn battery(minimum_mv: u32, maximum_mv: u32, power_mw: u32) -> u32 {
    (0b01 << 30) | ((maximum_mv / 50) << 20) | ((minimum_mv / 50) << 10) | (power_mw / 250)
}

fn variable(minimum_mv: u32, maximum_mv: u32, current_ma: u32) -> u32 {
    (0b10 << 30) | ((maximum_mv / 50) << 20) | ((minimum_mv / 50) << 10) | (current_ma / 10)
}

fn pps(minimum_mv: u32, maximum_mv: u32, current_ma: u32, power_limited: bool) -> u32 {
    (0b11 << 30)
        | (u32::from(power_limited) << 27)
        | ((maximum_mv / 100) << 17)
        | ((minimum_mv / 100) << 8)
        | (current_ma / 50)
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

fn spr_capabilities(pdos: &[u32]) -> SourceCapabilities {
    SourceCapabilities::new(CapabilitiesKind::Spr, pdos).unwrap()
}

fn epr_capabilities(position_2: u32, position_8: u32) -> SourceCapabilities {
    SourceCapabilities::new(CapabilitiesKind::Epr, &[fixed(5_000, 3_000, true), position_2, 0, 0, 0, 0, 0, position_8])
        .unwrap()
}

#[test]
fn request_plan_compacts_without_losing_public_semantics() {
    assert_eq!(core::mem::size_of::<pd_sink::RequestPlan>(), 24);

    let fixed_caps = spr_capabilities(&[fixed(5_000, 3_000, false)]);
    let oversized = RequestPlanner::new()
        .for_pdo(&fixed_caps, PortMode::Spr, 1, Demand::Current(Milliamps(100_000)), RequestContext::default())
        .unwrap();
    assert_eq!(oversized.object_position(), 1);
    assert_eq!(oversized.supply(), SupplyKind::Fixed);
    assert_eq!(oversized.message(), RequestMessage::Request);
    assert_eq!(oversized.pdo_copy(), None);
    assert_eq!(oversized.voltage(), PlannedVoltage::Fixed(Millivolts(5_000)));
    assert_eq!(
        oversized.operating(),
        PlannedOperating::Current {
            requested: Some(Milliamps(100_000)),
            source_limit: Milliamps(3_000),
            operating: Milliamps(3_000),
            confidence: CurrentConfidence::Advertised,
            limited_by: LimitReason::Source,
        }
    );
    assert!(oversized.capability_mismatch());
    let pps_caps = spr_capabilities(&[fixed(5_000, 3_000, false), pps(5_000, 21_000, 5_000, false)]);
    let adjustable = RequestPlanner::new()
        .for_pdo(
            &pps_caps,
            PortMode::Spr,
            2,
            Demand::Adjustable { voltage: Millivolts(19_419), current: Some(Milliamps(3_333)) },
            RequestContext::default(),
        )
        .unwrap();
    assert_eq!(
        adjustable.voltage(),
        PlannedVoltage::Adjustable { requested: Millivolts(19_419), encoded: Millivolts(19_400), step_mv: 20 }
    );
    assert_eq!(
        adjustable.operating(),
        PlannedOperating::Current {
            requested: Some(Milliamps(3_333)),
            source_limit: Milliamps(5_000),
            operating: Milliamps(3_300),
            confidence: CurrentConfidence::Advertised,
            limited_by: LimitReason::User,
        }
    );

    let epr_pdo = fixed(48_000, 5_000, false);
    let epr_caps = epr_capabilities(fixed(9_000, 3_000, false), epr_pdo);
    let epr =
        RequestPlanner::new().for_pdo(&epr_caps, PortMode::Epr, 8, Demand::Maximum, RequestContext::default()).unwrap();
    assert_eq!(epr.message(), RequestMessage::EprRequest);
    assert_eq!(epr.pdo_copy(), Some(epr_pdo));
    assert_eq!(epr.data_objects(), ([epr.rdo(), epr_pdo], 2));
}

#[test]
fn capability_parser_retains_unsupported_legacy_source_pdos() {
    let capabilities = SourceCapabilities::new(
        CapabilitiesKind::Epr,
        &[
            fixed(5_000, 3_000, true),
            battery(5_000, 12_000, 45_000),
            variable(5_000, 20_000, 3_000),
            pps(5_000, 21_000, 5_000, true),
            spr_avs(4_000, 3_000),
            0,
            0,
            fixed(28_000, 5_000, false),
            epr_avs(48_000, 140_000),
        ],
    )
    .unwrap();

    assert!(capabilities.epr_mode_capable());
    assert_eq!(capabilities.len(), 9);
    assert!(matches!(capabilities.pdo(1).unwrap().supply, SourceSupply::Fixed(_)));
    assert_eq!(capabilities.pdo(2).unwrap().validity, PdoValidity::Unsupported);
    assert!(matches!(
        capabilities.pdo(2).unwrap().supply,
        SourceSupply::Unsupported { pdo_type: 0b01, apdo_type: None }
    ));
    assert_eq!(capabilities.pdo(3).unwrap().validity, PdoValidity::Unsupported);
    assert!(matches!(
        capabilities.pdo(3).unwrap().supply,
        SourceSupply::Unsupported { pdo_type: 0b10, apdo_type: None }
    ));
    assert!(matches!(capabilities.pdo(4).unwrap().supply, SourceSupply::Pps(_)));
    assert!(matches!(capabilities.pdo(5).unwrap().supply, SourceSupply::SprAvs(_)));
    assert_eq!(capabilities.pdo(6).unwrap().validity, PdoValidity::ZeroPadding);
    assert!(matches!(capabilities.pdo(8).unwrap().supply, SourceSupply::Fixed(_)));
    assert!(matches!(capabilities.pdo(9).unwrap().supply, SourceSupply::EprAvs(_)));
}

#[test]
fn stack_bridge_preserves_raw_padding_and_reserved_pdos_for_one_policy_parse() {
    let reserved_augmented = 0xf123_4567;
    let raw = [fixed(5_000, 3_000, true), 0, 0, 0, 0, 0, 0, reserved_augmented];
    let stack = StackSourceCapabilities::new_with_raw_pdos(heapless::Vec::from_slice(&raw).expect("eight PDOs fit"));
    let capabilities = capabilities_from_stack(&stack).unwrap();

    assert_eq!(capabilities.kind(), CapabilitiesKind::Epr);
    assert_eq!(capabilities.raw_pdos(), raw);
    assert_eq!(capabilities.pdo(2).unwrap().validity, PdoValidity::ZeroPadding);
    assert_eq!(capabilities.pdo(8).unwrap().validity, PdoValidity::Unsupported);
    assert!(matches!(
        capabilities.pdo(8).unwrap().supply,
        SourceSupply::Unsupported { pdo_type: 0b11, apdo_type: Some(0b11) }
    ));
}

#[test]
fn capability_list_preserves_positions_and_rejects_impossible_lengths() {
    assert_eq!(SourceCapabilities::new(CapabilitiesKind::Spr, &[]), Err(CapabilityListError::Empty));
    assert_eq!(
        SourceCapabilities::new(CapabilitiesKind::Epr, &[fixed(5_000, 3_000, true); 7]),
        Err(CapabilityListError::EprListTooShort { supplied: 7 })
    );

    let malformed = spr_capabilities(&[fixed(5_000, 3_000, false), fixed(28_000, 5_000, false)]);
    assert_eq!(malformed.pdo(2).unwrap().validity, PdoValidity::Malformed(PdoError::InvalidPosition));
}

#[test]
fn bounded_noncanonical_pps_offers_remain_requestable_and_are_capped_safely() {
    let capabilities = spr_capabilities(&[
        fixed(5_000, 3_000, false),
        pps(4_500, 11_000, 5_000, false),
        pps(3_600, 21_000, 3_000, false),
        pps(3_600, 20_000, 6_100, false),
    ]);

    for position in 2..=4 {
        assert_eq!(capabilities.pdo(position).unwrap().validity, PdoValidity::Compatible);
    }

    let planner = RequestPlanner::new();
    let aohi_style = planner
        .for_pdo(
            &capabilities,
            PortMode::Spr,
            3,
            Demand::Adjustable { voltage: Millivolts(19_400), current: None },
            RequestContext::default(),
        )
        .unwrap();
    assert_eq!(aohi_style.object_position(), 3);
    assert_eq!(aohi_style.operating_current(), Milliamps(3_000));

    let proprietary_high_current = planner
        .for_pdo(
            &capabilities,
            PortMode::Spr,
            4,
            Demand::Adjustable { voltage: Millivolts(19_400), current: None },
            RequestContext::default(),
        )
        .unwrap();
    assert!(matches!(
        proprietary_high_current.operating(),
        PlannedOperating::Current {
            source_limit: Milliamps(5_000),
            operating: Milliamps(5_000),
            limited_by: LimitReason::Protocol,
            ..
        }
    ));
    assert_eq!(proprietary_high_current.rdo() & 0x7f, 100, "the wire request must never exceed 5 A");
}

#[test]
fn deprecated_three_point_three_volt_pps_endpoint_remains_requestable() {
    let capabilities = spr_capabilities(&[fixed(5_000, 3_000, false), pps(3_300, 11_000, 3_000, false)]);
    assert_eq!(capabilities.pdo(2).unwrap().validity, PdoValidity::Valid);

    let plan = RequestPlanner::new()
        .for_voltage(&capabilities, PortMode::Spr, Millivolts(3_300), None, Preference::Pps, RequestContext::default())
        .unwrap();
    assert_eq!(
        plan.voltage(),
        PlannedVoltage::Adjustable { requested: Millivolts(3_300), encoded: Millivolts(3_300), step_mv: 20 }
    );
    assert_eq!((plan.rdo() >> 9) & 0xfff, 165);
}

#[test]
fn aohi_five_to_twenty_eight_volt_epr_avs_is_compatible_and_opt_in_below_fifteen_volts() {
    let aohi_avs = epr_avs_range(5_000, 28_000, 140_000);
    assert_eq!(aohi_avs, 0xd230_328c);
    let capabilities = SourceCapabilities::new(
        CapabilitiesKind::Epr,
        &[fixed(5_000, 3_000, true), 0, 0, 0, 0, 0, 0, fixed(28_000, 5_000, false), aohi_avs],
    )
    .unwrap();
    let pdo = capabilities.pdo(9).unwrap();

    assert_eq!(pdo.validity, PdoValidity::Compatible);
    assert_eq!(pdo.voltage_range(), Some((Millivolts(5_000), Millivolts(28_000))));
    assert_eq!(pdo.standard_voltage_range(), Some((Millivolts(15_000), Millivolts(28_000))));

    let planner = RequestPlanner::new();
    assert_eq!(
        planner.for_voltage(
            &capabilities,
            PortMode::Epr,
            Millivolts(10_000),
            None,
            Preference::EprAvs,
            RequestContext::default(),
        ),
        Err(PlanError::VoltageUnavailable(Millivolts(10_000)))
    );

    let standard = planner
        .for_voltage(
            &capabilities,
            PortMode::Epr,
            Millivolts(15_000),
            None,
            Preference::EprAvs,
            RequestContext::default(),
        )
        .unwrap();
    assert_eq!(standard.object_position(), 9);

    let nonstandard = planner
        .for_voltage(
            &capabilities,
            PortMode::Epr,
            Millivolts(10_000),
            None,
            Preference::EprAvsNonstandard,
            RequestContext::default(),
        )
        .unwrap();
    assert_eq!(nonstandard.object_position(), 9);
    assert_eq!(nonstandard.pdo_copy(), Some(0xd230_328c));
    assert_eq!((nonstandard.rdo() >> 9) & 0xfff, 400);
    assert_eq!(nonstandard.rdo() & 0x7f, 100, "the non-standard range remains capped at 5 A");
}

#[test]
fn unsafe_epr_avs_extensions_remain_malformed() {
    for raw in [epr_avs_range(4_900, 28_000, 140_000), epr_avs_range(15_000, 50_100, 140_000)] {
        let capabilities = epr_capabilities(0, raw);
        assert_eq!(capabilities.pdo(8).unwrap().validity, PdoValidity::Malformed(PdoError::InvalidVoltageRange));
    }
}

#[test]
fn advertised_fifty_volt_epr_avs_is_compatible_and_explicit() {
    let capabilities = epr_capabilities(0, epr_avs_range(15_000, 50_000, 140_000));
    let pdo = capabilities.pdo(8).unwrap();
    assert_eq!(pdo.validity, PdoValidity::Compatible);
    assert_eq!(pdo.voltage_range(), Some((Millivolts(15_000), Millivolts(50_000))));
    assert_eq!(pdo.standard_voltage_range(), Some((Millivolts(15_000), Millivolts(48_000))));

    let planner = RequestPlanner::new();
    assert_eq!(
        planner.for_voltage(
            &capabilities,
            PortMode::Epr,
            Millivolts(50_000),
            None,
            Preference::EprAvs,
            RequestContext {
                limits: SinkLimits { max_voltage: Some(Millivolts(50_000)), ..SinkLimits::default() },
                ..RequestContext::default()
            },
        ),
        Err(PlanError::VoltageUnavailable(Millivolts(50_000)))
    );
    assert_eq!(
        planner.for_voltage(
            &capabilities,
            PortMode::Epr,
            Millivolts(50_000),
            None,
            Preference::EprAvsNonstandard,
            RequestContext::default(),
        ),
        Err(PlanError::VoltageAboveSinkLimit { requested: Millivolts(50_000), maximum: Millivolts(48_000) })
    );

    let plan = planner
        .for_voltage(
            &capabilities,
            PortMode::Epr,
            Millivolts(50_000),
            None,
            Preference::EprAvsNonstandard,
            RequestContext {
                limits: SinkLimits { max_voltage: Some(Millivolts(50_000)), ..SinkLimits::default() },
                ..RequestContext::default()
            },
        )
        .unwrap();
    assert_eq!(
        plan.voltage(),
        PlannedVoltage::Adjustable { requested: Millivolts(50_000), encoded: Millivolts(50_000), step_mv: 100 }
    );
    assert_eq!((plan.rdo() >> 9) & 0xfff, 2_000);
    assert_eq!(plan.rdo() & 0x7f, 56, "140 W at 50 V is limited to 2.8 A");

    match request_to_stack(plan).unwrap() {
        PowerSource::EprRequest(request) => {
            assert_eq!(request.rdo, plan.rdo());
            assert_eq!(request.pdo, epr_avs_range(15_000, 50_000, 140_000));
        }
        _ => panic!("50 V AVS must remain a two-object EPR Request"),
    }
}

#[test]
fn any_bounded_fifteen_to_forty_eight_volt_avs_range_is_standard() {
    let capabilities = epr_capabilities(0, epr_avs_range(20_000, 47_500, 140_000));
    let pdo = capabilities.pdo(8).unwrap();
    assert_eq!(pdo.validity, PdoValidity::Valid);
    assert_eq!(pdo.standard_voltage_range(), Some((Millivolts(20_000), Millivolts(47_500))));
}

#[test]
fn unsafe_pps_ranges_remain_malformed_and_unrequestable() {
    let capabilities = spr_capabilities(&[
        fixed(5_000, 3_000, false),
        pps(3_200, 11_000, 3_000, false),
        pps(3_300, 21_100, 3_000, false),
        pps(3_300, 11_000, 0, false),
    ]);

    assert_eq!(capabilities.pdo(2).unwrap().validity, PdoValidity::Malformed(PdoError::InvalidVoltageRange));
    assert_eq!(capabilities.pdo(3).unwrap().validity, PdoValidity::Malformed(PdoError::InvalidVoltageRange));
    assert_eq!(capabilities.pdo(4).unwrap().validity, PdoValidity::Malformed(PdoError::InvalidCurrent));
    assert!(matches!(
        RequestPlanner::new().for_pdo(
            &capabilities,
            PortMode::Spr,
            2,
            Demand::Adjustable { voltage: Millivolts(5_000), current: None },
            RequestContext::default(),
        ),
        Err(PlanError::MalformedPdo { position: 2, error: PdoError::InvalidVoltageRange })
    ));
}

#[test]
fn direct_adjustable_requests_cannot_quantize_back_into_an_offer() {
    let capabilities = spr_capabilities(&[fixed(5_000, 3_000, false), pps(3_600, 21_000, 3_000, false)]);
    let planner = RequestPlanner::new();

    for requested in [3_599, 21_001] {
        assert_eq!(
            planner.for_pdo(
                &capabilities,
                PortMode::Spr,
                2,
                Demand::Adjustable { voltage: Millivolts(requested), current: None },
                RequestContext::default(),
            ),
            Err(PlanError::VoltageOutsideOffer { position: 2, requested: Millivolts(requested) })
        );
    }
}

#[test]
fn fixed_rdo_never_inverts_operating_and_maximum_fields() {
    let capabilities = spr_capabilities(&[fixed(5_000, 3_000, false)]);
    let plan = RequestPlanner::new()
        .for_pdo(&capabilities, PortMode::Spr, 1, Demand::Current(Milliamps(1_500)), RequestContext::default())
        .unwrap();
    let operating = (plan.rdo() >> 10) & 0x3ff;
    let maximum = plan.rdo() & 0x3ff;
    assert_ne!(operating, 0);
    assert!(operating <= maximum, "RDO operating field must not exceed its maximum field");
}

#[test]
fn pps_19_4_volts_is_exact_and_uses_an_ordinary_request() {
    let pps_raw = pps(5_000, 21_000, 3_000, false);
    let capabilities = spr_capabilities(&[fixed(5_000, 3_000, true), pps_raw]);
    let plan = RequestPlanner::new()
        .for_voltage(
            &capabilities,
            PortMode::Spr,
            Millivolts(19_400),
            None,
            Preference::Auto,
            RequestContext::default(),
        )
        .unwrap();

    assert_eq!(plan.supply(), SupplyKind::Pps);
    assert_eq!(plan.message(), RequestMessage::Request);
    assert_eq!(plan.pdo_copy(), None);
    assert_eq!(plan.rdo() >> 28, 2);
    assert_eq!((plan.rdo() >> 9) & 0xfff, 970);
    assert_eq!(plan.rdo() & 0x7f, 60);
    assert_eq!(plan.rdo() & (1 << 25), 0, "power-only product must clear USB communications");
    assert_eq!(
        plan.voltage(),
        PlannedVoltage::Adjustable { requested: Millivolts(19_400), encoded: Millivolts(19_400), step_mv: 20 }
    );
}

#[test]
fn auto_prefers_an_exact_fixed_pdo_over_an_adjustable_offer() {
    let capabilities =
        spr_capabilities(&[fixed(5_000, 3_000, false), fixed(20_000, 5_000, false), pps(5_000, 21_000, 3_000, false)]);
    let plan = RequestPlanner::new()
        .for_voltage(
            &capabilities,
            PortMode::Spr,
            Millivolts(20_000),
            None,
            Preference::Auto,
            RequestContext::default(),
        )
        .unwrap();

    assert_eq!(plan.object_position(), 2);
    assert_eq!(plan.supply(), SupplyKind::Fixed);
}

#[test]
fn epr_avs_19_4_volts_caps_a_140_watt_offer_at_five_amps() {
    let avs_raw = epr_avs(48_000, 140_000);
    let capabilities = epr_capabilities(0, avs_raw);
    let plan = RequestPlanner::new()
        .for_voltage(
            &capabilities,
            PortMode::Epr,
            Millivolts(19_400),
            None,
            Preference::EprAvs,
            RequestContext::default(),
        )
        .unwrap();

    assert_eq!(plan.message(), RequestMessage::EprRequest);
    assert_eq!(plan.pdo_copy(), Some(avs_raw));
    assert_eq!((plan.rdo() >> 9) & 0xfff, 776);
    assert_eq!(((plan.rdo() >> 9) & 0xfff) & 0x3, 0);
    assert_eq!(plan.rdo() & 0x7f, 100);
    assert_eq!(plan.operating_current(), Milliamps(5_000));
    assert!(matches!(
        plan.operating(),
        PlannedOperating::Current {
            source_limit: Milliamps(5_000),
            confidence: CurrentConfidence::DerivedFromPdoPdp,
            ..
        }
    ));
    assert_eq!(plan.data_objects(), ([plan.rdo(), avs_raw], 2));
}

#[test]
fn source_info_present_pdp_caps_fixed_and_epr_avs_current() {
    let context = RequestContext { source_present_pdp: Some(Milliwatts(60_000)), ..RequestContext::default() };

    let fixed_capabilities = epr_capabilities(0, fixed(48_000, 5_000, false));
    let fixed_plan =
        RequestPlanner::new().for_pdo(&fixed_capabilities, PortMode::Epr, 8, Demand::Maximum, context).unwrap();
    assert!(matches!(
        fixed_plan.operating(),
        PlannedOperating::Current {
            source_limit: Milliamps(1_250),
            operating: Milliamps(1_250),
            confidence: CurrentConfidence::DerivedFromSourceInfo,
            limited_by: LimitReason::Source,
            ..
        }
    ));

    let avs_capabilities = epr_capabilities(0, epr_avs(48_000, 140_000));
    let avs_plan = RequestPlanner::new()
        .for_voltage(&avs_capabilities, PortMode::Epr, Millivolts(19_400), None, Preference::EprAvs, context)
        .unwrap();
    assert!(matches!(
        avs_plan.operating(),
        PlannedOperating::Current {
            source_limit: Milliamps(3_050),
            operating: Milliamps(3_050),
            confidence: CurrentConfidence::DerivedFromSourceInfo,
            limited_by: LimitReason::Source,
            ..
        }
    ));
    assert_eq!(avs_plan.rdo() & 0x7f, 61);
}

#[test]
fn epr_avs_current_is_rounded_down_to_fifty_milliamps() {
    let capabilities = epr_capabilities(0, epr_avs(28_000, 70_000));
    let plan = RequestPlanner::new()
        .for_voltage(
            &capabilities,
            PortMode::Epr,
            Millivolts(19_400),
            None,
            Preference::EprAvs,
            RequestContext::default(),
        )
        .unwrap();

    assert_eq!(plan.operating_current(), Milliamps(3_600));
    assert_eq!(plan.rdo() & 0x7f, 72);
}

#[test]
fn epr_mode_always_uses_epr_request_even_for_an_spr_pps_position() {
    let pps_raw = pps(5_000, 21_000, 5_000, false);
    let capabilities = epr_capabilities(pps_raw, fixed(48_000, 5_000, false));
    let plan = RequestPlanner::new()
        .for_voltage(
            &capabilities,
            PortMode::Epr,
            Millivolts(19_400),
            Some(Milliamps(3_000)),
            Preference::Position(2),
            RequestContext::default(),
        )
        .unwrap();

    assert_eq!(plan.message(), RequestMessage::EprRequest);
    assert_eq!(plan.pdo_copy(), Some(pps_raw));
    assert_eq!(plan.data_objects().1, 2);
}

#[test]
fn fixed_48_volts_is_only_planned_as_an_epr_request() {
    let pdo_48v = fixed(48_000, 5_000, false);
    let capabilities = epr_capabilities(0, pdo_48v);
    let planner = RequestPlanner::new();

    assert_eq!(
        planner.for_pdo(&capabilities, PortMode::Spr, 8, Demand::Maximum, RequestContext::default(),),
        Err(PlanError::EprModeRequired(8))
    );

    let plan = planner.for_pdo(&capabilities, PortMode::Epr, 8, Demand::Maximum, RequestContext::default()).unwrap();
    assert_eq!(plan.voltage(), PlannedVoltage::Fixed(Millivolts(48_000)));
    assert_eq!(plan.message(), RequestMessage::EprRequest);
    assert_eq!(plan.pdo_copy(), Some(pdo_48v));
}

#[test]
fn spr_avs_is_parsed_and_requested_with_100_mv_quantization() {
    let capabilities = spr_capabilities(&[fixed(5_000, 3_000, false), spr_avs(4_000, 3_000)]);
    let plan = RequestPlanner::new()
        .for_voltage(
            &capabilities,
            PortMode::Spr,
            Millivolts(19_450),
            None,
            Preference::SprAvs,
            RequestContext::default(),
        )
        .unwrap();

    assert_eq!(plan.supply(), SupplyKind::SprAvs);
    assert_eq!((plan.rdo() >> 9) & 0xfff, 776);
    assert_eq!(plan.operating_current(), Milliamps(3_000));
    assert_eq!(
        plan.voltage(),
        PlannedVoltage::Adjustable { requested: Millivolts(19_450), encoded: Millivolts(19_400), step_mv: 100 }
    );
}

#[test]
fn power_limited_pps_reports_uncertainty_until_source_info_is_known() {
    let capabilities = spr_capabilities(&[fixed(5_000, 3_000, false), pps(5_000, 21_000, 5_000, true)]);
    let planner = RequestPlanner::new();

    let unknown = planner
        .for_voltage(&capabilities, PortMode::Spr, Millivolts(20_000), None, Preference::Pps, RequestContext::default())
        .unwrap();
    assert!(matches!(
        unknown.operating(),
        PlannedOperating::Current {
            confidence: CurrentConfidence::PowerLimitedUpperBound,
            operating: Milliamps(5_000),
            ..
        }
    ));

    let known = planner
        .for_voltage(
            &capabilities,
            PortMode::Spr,
            Millivolts(20_000),
            None,
            Preference::Pps,
            RequestContext { source_present_pdp: Some(Milliwatts(60_000)), ..RequestContext::default() },
        )
        .unwrap();
    assert!(matches!(
        known.operating(),
        PlannedOperating::Current {
            confidence: CurrentConfidence::DerivedFromSourceInfo,
            operating: Milliamps(3_000),
            ..
        }
    ));
}

#[test]
fn requested_and_local_limits_are_applied_before_encoding() {
    let capabilities = spr_capabilities(&[fixed(5_000, 5_000, false), pps(5_000, 21_000, 5_000, false)]);
    let plan = RequestPlanner::new()
        .for_voltage(
            &capabilities,
            PortMode::Spr,
            Millivolts(20_000),
            Some(Milliamps(5_000)),
            Preference::Pps,
            RequestContext {
                limits: SinkLimits {
                    board_max_current: Some(Milliamps(4_000)),
                    cable_max_current: Some(Milliamps(3_000)),
                    max_power: Some(Milliwatts(50_000)),
                    max_voltage: Some(Millivolts(48_000)),
                },
                ..RequestContext::default()
            },
        )
        .unwrap();

    assert!(matches!(
        plan.operating(),
        PlannedOperating::Current { operating: Milliamps(2_500), limited_by: LimitReason::SinkPower, .. }
    ));
    assert_eq!(plan.rdo() & 0x7f, 50);
}

#[test]
fn over_limit_user_demand_sets_mismatch_but_encodes_a_valid_request() {
    let capabilities = spr_capabilities(&[fixed(5_000, 3_000, false), pps(5_000, 21_000, 3_000, false)]);
    let plan = RequestPlanner::new()
        .for_voltage(
            &capabilities,
            PortMode::Spr,
            Millivolts(19_400),
            Some(Milliamps(5_000)),
            Preference::Pps,
            RequestContext::default(),
        )
        .unwrap();

    assert!(plan.capability_mismatch());
    assert_ne!(plan.rdo() & (1 << 26), 0);
    assert_eq!(plan.rdo() & 0x7f, 60, "RDO current must be capped to the 3 A offer");
}

#[test]
fn variable_and_battery_pdos_are_retained_but_not_requestable() {
    let capabilities =
        spr_capabilities(&[fixed(5_000, 3_000, false), variable(5_000, 12_000, 2_000), battery(5_000, 12_000, 45_000)]);
    let planner = RequestPlanner::new();

    assert_eq!(
        planner.for_pdo(&capabilities, PortMode::Spr, 2, Demand::Maximum, RequestContext::default()),
        Err(PlanError::UnsupportedPdo(2))
    );
    assert_eq!(
        planner.for_pdo(&capabilities, PortMode::Spr, 3, Demand::Maximum, RequestContext::default()),
        Err(PlanError::UnsupportedPdo(3))
    );

    let invalid_first = spr_capabilities(&[battery(5_000, 12_000, 45_000)]);
    assert_eq!(invalid_first.pdo(1).unwrap().validity, PdoValidity::Malformed(PdoError::InvalidFirstPdo));

    let invalid_epr_position = epr_capabilities(0, variable(5_000, 12_000, 2_000));
    assert_eq!(invalid_epr_position.pdo(8).unwrap().validity, PdoValidity::Malformed(PdoError::InvalidPosition));
}

#[test]
fn direct_maximum_request_reaches_every_position_in_a_full_epr_list() {
    let raw = [
        fixed(5_000, 3_000, true),
        fixed(9_000, 3_000, false),
        fixed(12_000, 3_000, false),
        pps(5_000, 21_000, 3_000, false),
        spr_avs(4_000, 3_000),
        fixed(15_000, 3_000, false),
        fixed(20_000, 5_000, false),
        fixed(28_000, 5_000, false),
        fixed(36_000, 5_000, false),
        fixed(48_000, 5_000, false),
        epr_avs(48_000, 140_000),
    ];
    let capabilities = SourceCapabilities::new(CapabilitiesKind::Epr, &raw).unwrap();
    let planner = RequestPlanner::new();

    for position in 1..=11 {
        let plan = planner
            .for_pdo(&capabilities, PortMode::Epr, position, Demand::Maximum, RequestContext::default())
            .unwrap();

        assert_eq!(plan.object_position(), position);
        assert_eq!(plan.message(), RequestMessage::EprRequest);
        assert_eq!(plan.pdo_copy(), Some(raw[usize::from(position - 1)]));
        assert_eq!(plan.rdo() >> 28, u32::from(position));
        match plan.voltage() {
            PlannedVoltage::Fixed(voltage) => assert!(voltage <= Millivolts(48_000)),
            PlannedVoltage::Adjustable { requested, encoded, .. } => {
                assert_eq!(requested, encoded);
                assert!(encoded <= Millivolts(48_000));
            }
        }
        match plan.operating() {
            PlannedOperating::Current { operating, .. } => assert!(operating > Milliamps(0)),
        }
    }
}

#[test]
fn every_adjustable_family_accepts_its_minimum_and_maximum_voltage() {
    let spr = spr_capabilities(&[fixed(5_000, 3_000, true), pps(3_600, 21_000, 3_000, false), spr_avs(4_000, 3_000)]);
    let epr = epr_capabilities(0, epr_avs(48_000, 140_000));
    let planner = RequestPlanner::new();

    for (capabilities, mode, position, minimum, maximum, step_mv, wire_unit_mv) in [
        (spr, PortMode::Spr, 2, 3_600, 21_000, 20, 20),
        (spr, PortMode::Spr, 3, 9_000, 20_000, 100, 25),
        (epr, PortMode::Epr, 8, 15_000, 48_000, 100, 25),
    ] {
        for requested_mv in [minimum, maximum] {
            let plan = planner
                .for_pdo(
                    &capabilities,
                    mode,
                    position,
                    Demand::Adjustable { voltage: Millivolts(requested_mv), current: None },
                    RequestContext::default(),
                )
                .unwrap();

            assert_eq!(
                plan.voltage(),
                PlannedVoltage::Adjustable {
                    requested: Millivolts(requested_mv),
                    encoded: Millivolts(requested_mv),
                    step_mv,
                }
            );
            assert_eq!((plan.rdo() >> 9) & 0xfff, requested_mv / wire_unit_mv);
        }
    }
}

#[test]
fn adjustable_maximum_does_not_bypass_the_sink_voltage_limit() {
    let capabilities = spr_capabilities(&[fixed(5_000, 3_000, false), pps(5_000, 21_000, 3_000, false)]);

    assert_eq!(
        RequestPlanner::new().for_pdo(
            &capabilities,
            PortMode::Spr,
            2,
            Demand::Maximum,
            RequestContext {
                limits: SinkLimits { max_voltage: Some(Millivolts(20_000)), ..SinkLimits::default() },
                ..RequestContext::default()
            },
        ),
        Err(PlanError::VoltageAboveSinkLimit { requested: Millivolts(21_000), maximum: Millivolts(20_000) })
    );
}

#[test]
fn contract_is_not_confirmed_until_ps_rdy_and_detach_clears_it() {
    let capabilities = spr_capabilities(&[fixed(5_000, 3_000, true)]);
    let plan = RequestPlanner::new()
        .for_voltage(&capabilities, PortMode::Spr, Millivolts(5_000), None, Preference::Auto, RequestContext::default())
        .unwrap();
    let mut contract = ContractTracker::new();

    assert_eq!(contract.state(), ContractState::Detached);
    assert!(!contract.load_may_enable());
    contract.on_attach();
    contract.on_capabilities().unwrap();
    contract.on_request(plan).unwrap();
    assert_eq!(contract.confirmed_current(), None);
    contract.on_accept().unwrap();
    assert_eq!(contract.confirmed_current(), None);
    contract.on_ps_ready().unwrap();
    assert_eq!(contract.state(), ContractState::Ready);
    assert_eq!(contract.confirmed_current(), Some(Milliamps(3_000)));
    assert!(contract.load_may_enable());

    contract.on_detach();
    assert_eq!(contract.state(), ContractState::Detached);
    assert_eq!(contract.confirmed_current(), None);
    assert!(!contract.load_may_enable());
}

#[test]
fn rejected_renegotiation_restores_the_previous_ready_contract() {
    let capabilities = spr_capabilities(&[fixed(5_000, 3_000, true), pps(5_000, 21_000, 2_000, false)]);
    let planner = RequestPlanner::new();
    let initial = planner
        .for_voltage(
            &capabilities,
            PortMode::Spr,
            Millivolts(5_000),
            None,
            Preference::Fixed,
            RequestContext::default(),
        )
        .unwrap();
    let renegotiation = planner
        .for_voltage(&capabilities, PortMode::Spr, Millivolts(19_400), None, Preference::Pps, RequestContext::default())
        .unwrap();
    let mut contract = ContractTracker::new();

    contract.on_attach();
    contract.on_capabilities().unwrap();
    contract.on_request(initial).unwrap();
    contract.on_accept().unwrap();
    contract.on_ps_ready().unwrap();
    assert_eq!(contract.active_plan(), Some(initial));
    assert_eq!(contract.classify_transition(initial).kind, ContractTransitionKind::IdenticalRefresh);
    assert_eq!(contract.classify_transition(renegotiation).kind, ContractTransitionKind::VoltageChange);

    contract.on_request(renegotiation).unwrap();
    assert_eq!(contract.confirmed_current(), None);
    contract.on_reject_or_wait();

    assert_eq!(contract.state(), ContractState::Ready);
    assert_eq!(contract.active_plan(), Some(initial));
    assert_eq!(contract.confirmed_current(), Some(Milliamps(3_000)));

    contract.on_request(renegotiation).unwrap();
    contract.on_accept().unwrap();
    contract.on_ps_ready().unwrap();
    let same_wire = planner
        .for_voltage(&capabilities, PortMode::Spr, Millivolts(19_401), None, Preference::Pps, RequestContext::default())
        .unwrap();
    assert_ne!(renegotiation, same_wire, "reported request metadata should retain the user's exact voltage");
    assert_eq!(renegotiation.rdo(), same_wire.rdo(), "both voltages quantize to the same PPS RDO");
    assert_eq!(
        contract.classify_transition(same_wire).kind,
        ContractTransitionKind::IdenticalRefresh,
        "wire-identical maintenance must keep the load stable"
    );
}

#[test]
fn transition_classification_uses_the_confirmed_rdo_current() {
    let capabilities = spr_capabilities(&[
        fixed(5_000, 5_000, true),
        fixed(9_000, 5_000, false),
        pps(5_000, 11_000, 3_000, false),
        pps(5_000, 11_000, 1_500, false),
        pps(5_000, 11_000, 4_000, true),
    ]);
    let planner = RequestPlanner::new();
    let context = RequestContext::default();
    let active = planner
        .for_voltage(
            &capabilities,
            PortMode::Spr,
            Millivolts(5_000),
            Some(Milliamps(2_000)),
            Preference::Fixed,
            context,
        )
        .unwrap();
    let mut contract = ContractTracker::new();
    assert_eq!(contract.classify_transition(active).kind, ContractTransitionKind::NoConfirmedContract);
    contract.on_attach();
    contract.on_capabilities().unwrap();
    contract.on_request(active).unwrap();
    contract.on_accept().unwrap();
    contract.on_ps_ready().unwrap();

    let sufficient = planner
        .for_pdo(
            &capabilities,
            PortMode::Spr,
            3,
            Demand::Adjustable { voltage: Millivolts(5_000), current: Some(Milliamps(3_000)) },
            context,
        )
        .unwrap();
    let reduced = planner
        .for_pdo(
            &capabilities,
            PortMode::Spr,
            4,
            Demand::Adjustable { voltage: Millivolts(5_000), current: Some(Milliamps(1_500)) },
            context,
        )
        .unwrap();
    let unknown = planner
        .for_pdo(
            &capabilities,
            PortMode::Spr,
            5,
            Demand::Adjustable { voltage: Millivolts(5_000), current: Some(Milliamps(3_000)) },
            context,
        )
        .unwrap();
    let voltage_change = planner
        .for_voltage(
            &capabilities,
            PortMode::Spr,
            Millivolts(9_000),
            Some(Milliamps(2_000)),
            Preference::Fixed,
            context,
        )
        .unwrap();

    assert_eq!(
        contract.classify_transition(sufficient).kind,
        ContractTransitionKind::SameVoltageSufficientCurrent,
        "a 3 A candidate is sufficient for the 2 A encoded active RDO even though its PDO advertised 5 A"
    );
    assert!(!contract.classify_transition(sufficient).inhibits_load());
    assert_eq!(contract.classify_transition(reduced).kind, ContractTransitionKind::SameVoltageReducedCurrent);
    assert!(contract.classify_transition(reduced).inhibits_load());
    assert_eq!(contract.classify_transition(unknown).kind, ContractTransitionKind::SameVoltageUnknownCurrent);
    assert!(contract.classify_transition(unknown).inhibits_load());
    assert_eq!(contract.classify_transition(voltage_change).kind, ContractTransitionKind::VoltageChange);
    assert!(contract.classify_transition(voltage_change).inhibits_load());
}

#[test]
fn safety_supervisor_debounces_attach_but_cuts_the_load_on_the_first_bad_sample() {
    let mut supervisor = PortSupervisor::new(SafetyTimings::default());
    let connected = PortInputs { cc_attached: true, vbus_present: true, hardware_ok: true };

    assert_eq!(supervisor.update(0, connected).state, PortState::AttachDebounce { since_ms: 0 });
    assert!(!supervisor.update(99, connected).attach_confirmed);
    assert!(supervisor.update(100, connected).attach_confirmed);
    assert!(supervisor.mark_contract_ready());

    let disconnected = PortInputs { cc_attached: false, ..connected };
    let first_bad_sample = supervisor.update(101, disconnected);
    assert!(first_bad_sample.force_cutoff);
    assert!(!first_bad_sample.load_enable);
    assert!(!first_bad_sample.detach_confirmed);
    assert_eq!(first_bad_sample.state, PortState::DetachDebounce { since_ms: 101 });

    let confirmed = supervisor.update(106, disconnected);
    assert!(confirmed.detach_confirmed);
    assert!(confirmed.restart_pd_stack);
    assert_eq!(confirmed.state, PortState::Unattached);
}

#[test]
fn a_detach_bounce_never_restores_the_old_contract() {
    let timings = SafetyTimings { attach_debounce_ms: 10, detach_confirm_ms: 5 };
    let mut supervisor = PortSupervisor::new(timings);
    let connected = PortInputs { cc_attached: true, vbus_present: true, hardware_ok: true };
    supervisor.update(0, connected);
    supervisor.update(10, connected);
    assert!(supervisor.mark_contract_ready());

    supervisor.update(11, PortInputs { vbus_present: false, ..connected });
    let restored = supervisor.update(12, connected);

    assert_eq!(restored.state, PortState::AttachDebounce { since_ms: 12 });
    assert!(!restored.load_enable);
    assert!(!supervisor.load_may_enable());
}
