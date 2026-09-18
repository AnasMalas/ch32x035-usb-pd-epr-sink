#[cfg(not(feature = "rich-telemetry"))]
use pd_sink::ControlCommandDecodeError;
use pd_sink::{
    decode_control_command, encode_control_event, encode_control_frame, Command, CommandStatus, ControlCommandKind,
    ControlCommandStreamDecoder, ControlEvent, ControlFrameDecoder, ControlFrameError, ControlPlanStage, Demand,
    DeviceInfo, HardResetCause, HardResetDirection, Milliamps, Millivolts, Milliwatts, Preference, UserRequest,
    CONTROL_MAX_FRAME_LEN, CONTROL_PROTOCOL_VERSION,
};
#[cfg(feature = "rich-telemetry")]
use pd_sink::{
    encode_control_event_packet, CapabilitiesKind, ContractOperatingPoint, ContractTransition, ContractTransitionKind,
    ControlEprEvent, ControlEventKind, ControlIntegrationError, ControlLifecycleEvent, ControllerError,
    EprEntryRefusal, EprExitRefusal, PdoValidity, PortMode, PpsStatus, RequestContext, RequestFlags, RequestPlanner,
    RequestResult, SinkLimits, SourceAlert, SourceCapabilities, SourceStatus, StatusQuery, StatusQueryFailure,
};

fn decode_one(bytes: &[u8]) -> pd_sink::ControlFrame {
    let mut decoder = ControlFrameDecoder::new();
    let mut decoded = None;
    for &byte in bytes {
        if let Some(result) = decoder.push(byte) {
            assert!(decoded.is_none());
            decoded = Some(result.unwrap());
        }
    }
    decoded.expect("complete frame")
}

fn decode_stream_command(bytes: &[u8]) -> pd_sink::StreamDecodedCommand {
    let mut decoder = ControlCommandStreamDecoder::new();
    let mut decoded = None;
    for &byte in bytes {
        if let Some(result) = decoder.push(byte) {
            assert!(decoded.is_none());
            decoded = Some(result);
        }
    }
    decoded.expect("complete command frame")
}

#[test]
fn framed_commands_round_trip_through_stream_decoder() {
    let mut payload = [0; 9];
    payload[0..4].copy_from_slice(&17_200u32.to_le_bytes());
    payload[4..8].copy_from_slice(&2_300u32.to_le_bytes());
    payload[8] = 2;

    let mut bytes = [0; CONTROL_MAX_FRAME_LEN];
    let len = encode_control_frame(
        CONTROL_PROTOCOL_VERSION,
        ControlCommandKind::RequestVoltage as u8,
        37,
        &payload,
        &mut bytes,
    )
    .unwrap();
    assert_eq!(
        &bytes[..len],
        &[0x50, 0x44, 0x01, 0x10, 0x25, 0x09, 0x30, 0x43, 0x00, 0x00, 0xfc, 0x08, 0x00, 0x00, 0x02, 0xb4]
    );
    let decoded = decode_control_command(&decode_one(&bytes[..len])).unwrap();
    assert_eq!(decoded.sequence, 37);
    assert_eq!(
        decoded.command,
        Command::Request(UserRequest::Voltage {
            voltage: Millivolts(17_200),
            current: Some(Milliamps(2_300)),
            preference: Preference::Pps,
        })
    );
}

#[test]
fn pdo_adjustable_command_preserves_optional_current() {
    let mut payload = [0; 10];
    payload[0] = 11;
    payload[1] = 2;
    payload[2..6].copy_from_slice(&19_400u32.to_le_bytes());
    payload[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut bytes = [0; CONTROL_MAX_FRAME_LEN];
    let len =
        encode_control_frame(CONTROL_PROTOCOL_VERSION, ControlCommandKind::RequestPdo as u8, 1, &payload, &mut bytes)
            .unwrap();

    assert_eq!(
        decode_control_command(&decode_one(&bytes[..len])).unwrap().command,
        Command::Request(UserRequest::Pdo {
            position: 11,
            demand: Demand::Adjustable { voltage: Millivolts(19_400), current: None },
        })
    );
}

#[test]
fn output_latch_commands_extend_protocol_v1_without_changing_existing_ids() {
    assert_eq!(ControlCommandKind::PpsStatus as u8, 0x0b);
    assert_eq!(ControlCommandKind::RequestVoltage as u8, 0x10);
    for (kind, expected) in
        [(ControlCommandKind::OutputOn, Command::OutputOn), (ControlCommandKind::OutputOff, Command::OutputOff)]
    {
        let mut bytes = [0; CONTROL_MAX_FRAME_LEN];
        let len = encode_control_frame(CONTROL_PROTOCOL_VERSION, kind as u8, 9, &[], &mut bytes).unwrap();
        assert_eq!(decode_control_command(&decode_one(&bytes[..len])).unwrap().command, expected);
        assert_eq!(decode_stream_command(&bytes[..len]).command, Ok(expected));
    }
}

#[test]
fn decoder_rejects_corruption_and_recovers_for_the_next_frame() {
    let mut first = [0; CONTROL_MAX_FRAME_LEN];
    let first_len =
        encode_control_frame(CONTROL_PROTOCOL_VERSION, ControlCommandKind::Status as u8, 1, &[], &mut first).unwrap();
    first[first_len - 1] ^= 0x80;

    let mut second = [0; CONTROL_MAX_FRAME_LEN];
    let second_len =
        encode_control_frame(CONTROL_PROTOCOL_VERSION, ControlCommandKind::Capabilities as u8, 2, &[], &mut second)
            .unwrap();

    let mut decoder = ControlFrameDecoder::new();
    let mut results = std::vec::Vec::new();
    for &byte in first[..first_len].iter().chain(&second[..second_len]) {
        if let Some(result) = decoder.push(byte) {
            results.push(result);
        }
    }
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], Err(ControlFrameError::InvalidChecksum));
    assert_eq!(results[1].as_ref().unwrap().sequence, 2);
}

#[test]
#[cfg(feature = "rich-telemetry")]
fn all_epr_capabilities_fit_in_one_usb_packet() {
    let pdos = [
        0x0a91_912c,
        0x0012_d12c,
        0x0013_c12c,
        0x0014_b12c,
        0x0016_41f4,
        0xc9a4_3264,
        0,
        0x0018_c1f4,
        0x001b_41f4,
        0x001f_01f4,
        0xd7c0_96f0,
    ];
    let capabilities = SourceCapabilities::new(CapabilitiesKind::Epr, &pdos).unwrap();
    let mut bytes = [0; CONTROL_MAX_FRAME_LEN];
    let len = encode_control_event(ControlEvent::Capabilities(capabilities), 0, &mut bytes).unwrap();
    assert!(len <= CONTROL_MAX_FRAME_LEN);

    let frame = decode_one(&bytes[..len]);
    assert_eq!(frame.payload()[0..3], [1, 1, 11]);
    assert_eq!(frame.payload().len(), 3 + pdos.len() * 4);
}

#[test]
fn device_and_empty_contract_events_are_versioned_frames() {
    let mut bytes = [0; CONTROL_MAX_FRAME_LEN];
    let len = encode_control_event(
        ControlEvent::Device(DeviceInfo {
            uid: [0xcd, 0xab, 0x0b, 0x92, 0x7a, 0xbd, 0x52, 0xfb],
            flags: DeviceInfo::PPS_SUPPORTED | DeviceInfo::EPR_SUPPORTED | DeviceInfo::RICH_TELEMETRY_SUPPORTED,
            max_voltage: Millivolts(48_000),
            max_current: Milliamps(5_000),
            max_power: Milliwatts(140_000),
        }),
        9,
        &mut bytes,
    )
    .unwrap();
    let frame = decode_one(&bytes[..len]);
    assert_eq!(frame.version, CONTROL_PROTOCOL_VERSION);
    assert_eq!(frame.sequence, 9);
    assert_eq!(&frame.payload()[..8], &[0xcd, 0xab, 0x0b, 0x92, 0x7a, 0xbd, 0x52, 0xfb]);
    assert_eq!(frame.payload()[8], 0x07);

    let len = encode_control_event(ControlEvent::Plan { stage: ControlPlanStage::Contract, plan: None }, 0, &mut bytes)
        .unwrap();
    assert_eq!(decode_one(&bytes[..len]).payload(), &[ControlPlanStage::Contract as u8, 0]);
}

#[test]
fn command_result_keeps_the_request_sequence() {
    let mut bytes = [0; CONTROL_MAX_FRAME_LEN];
    let len = encode_control_event(ControlEvent::CommandResult(CommandStatus::Queued), 123, &mut bytes).unwrap();
    let frame = decode_one(&bytes[..len]);
    assert_eq!(frame.sequence, 123);
    assert_eq!(frame.payload(), &[CommandStatus::Queued as u8]);
}

#[test]
fn hard_reset_event_appends_a_stable_cause_code() {
    let mut bytes = [0; CONTROL_MAX_FRAME_LEN];
    let len = encode_control_event(
        ControlEvent::HardReset {
            direction: HardResetDirection::Sent,
            cause: HardResetCause::EprKeepAliveFailed,
            recovery_ms: 2_000,
        },
        0,
        &mut bytes,
    )
    .unwrap();

    assert_eq!(decode_one(&bytes[..len]).payload(), &[1, 0xd0, 0x07, 0, 0, 8]);
}

#[test]
#[cfg(feature = "rich-telemetry")]
fn status_commands_and_events_use_compact_stable_payloads() {
    let mut bytes = [0; CONTROL_MAX_FRAME_LEN];
    let len =
        encode_control_frame(CONTROL_PROTOCOL_VERSION, ControlCommandKind::SourceStatus as u8, 1, &[], &mut bytes)
            .unwrap();
    assert_eq!(decode_control_command(&decode_one(&bytes[..len])).unwrap().command, Command::RequestSourceStatus);

    let len = encode_control_frame(CONTROL_PROTOCOL_VERSION, ControlCommandKind::PpsStatus as u8, 2, &[], &mut bytes)
        .unwrap();
    assert_eq!(decode_control_command(&decode_one(&bytes[..len])).unwrap().command, Command::RequestPpsStatus);

    let pps = PpsStatus::from_raw_bytes([0x5c, 0x03, 46, 0x0a]);
    let len = encode_control_event(ControlEvent::PpsStatus(pps), 3, &mut bytes).unwrap();
    assert_eq!(decode_one(&bytes[..len]).payload(), &[0x5c, 0x03, 46, 0x0a]);

    let general = SourceStatus::from_raw_bytes([42, 22, 0, 0x10, 2, 0x22, 1], true);
    let len = encode_control_event(ControlEvent::SourceStatus(general), 4, &mut bytes).unwrap();
    assert_eq!(decode_one(&bytes[..len]).payload(), &[1, 42, 22, 0, 0x10, 2, 0x22, 1]);

    let len = encode_control_event(ControlEvent::SourceAlert(SourceAlert::from_raw(1 << 28)), 5, &mut bytes).unwrap();
    assert_eq!(decode_one(&bytes[..len]).payload(), &(1u32 << 28).to_le_bytes());

    let len = encode_control_event(
        ControlEvent::StatusQueryFailed { query: StatusQuery::Pps, failure: StatusQueryFailure::Timeout },
        6,
        &mut bytes,
    )
    .unwrap();
    assert_eq!(decode_one(&bytes[..len]).payload(), &[1, 3]);

    let len = encode_control_event(
        ControlEvent::StatusQueryFailed {
            query: StatusQuery::SourceInfo,
            failure: StatusQueryFailure::UnsupportedRevision,
        },
        7,
        &mut bytes,
    )
    .unwrap();
    assert_eq!(decode_one(&bytes[..len]).payload(), &[2, 4]);
}

#[test]
fn command_stream_decoder_matches_the_general_protocol_decoder() {
    let voltage_payload = [0x30, 0x43, 0, 0, 0xfc, 0x08, 0, 0, 2];
    let pdo_payload = [11, 2, 0xc8, 0x4b, 0, 0, 0xff, 0xff, 0xff, 0xff];
    let commands: &[(u8, &[u8])] = &[
        (ControlCommandKind::Device as u8, &[]),
        (ControlCommandKind::Capabilities as u8, &[]),
        (ControlCommandKind::Plans as u8, &[]),
        (ControlCommandKind::SourceInfo as u8, &[]),
        (ControlCommandKind::EnterEpr as u8, &[]),
        (ControlCommandKind::EprCapabilities as u8, &[]),
        (ControlCommandKind::ExitEpr as u8, &[]),
        (ControlCommandKind::Status as u8, &[]),
        (ControlCommandKind::Help as u8, &[]),
        (ControlCommandKind::SourceStatus as u8, &[]),
        (ControlCommandKind::PpsStatus as u8, &[]),
        (ControlCommandKind::RequestVoltage as u8, &voltage_payload),
        (ControlCommandKind::RequestPdo as u8, &pdo_payload),
    ];

    for &(kind, payload) in commands {
        let mut bytes = [0; CONTROL_MAX_FRAME_LEN];
        let len = encode_control_frame(CONTROL_PROTOCOL_VERSION, kind, 91, payload, &mut bytes).unwrap();
        let expected = decode_control_command(&decode_one(&bytes[..len]));
        let streamed = decode_stream_command(&bytes[..len]);
        assert_eq!(streamed.sequence, 91);
        assert_eq!(streamed.command, expected.map(|decoded| decoded.command));
    }
}

#[test]
fn command_stream_decoder_preserves_error_classification_and_recovery() {
    let cases: &[(u8, u8, &[u8])] = &[
        (CONTROL_PROTOCOL_VERSION + 1, ControlCommandKind::Status as u8, &[]),
        (CONTROL_PROTOCOL_VERSION, 0x7f, &[0; 11]),
        (CONTROL_PROTOCOL_VERSION, ControlCommandKind::Status as u8, &[0; 11]),
        (CONTROL_PROTOCOL_VERSION, ControlCommandKind::RequestVoltage as u8, &[0; 10]),
    ];

    for &(version, kind, payload) in cases {
        let mut bytes = [0; CONTROL_MAX_FRAME_LEN];
        let len = encode_control_frame(version, kind, 47, payload, &mut bytes).unwrap();
        let expected = decode_control_command(&decode_one(&bytes[..len]));
        let streamed = decode_stream_command(&bytes[..len]);
        assert_eq!(streamed.sequence, 47);
        assert_eq!(streamed.command, expected.map(|decoded| decoded.command));
    }

    let mut damaged = [0; CONTROL_MAX_FRAME_LEN];
    let damaged_len =
        encode_control_frame(CONTROL_PROTOCOL_VERSION, ControlCommandKind::Status as u8, 1, &[], &mut damaged).unwrap();
    damaged[damaged_len - 1] ^= 0x80;
    let mut good = [0; CONTROL_MAX_FRAME_LEN];
    let good_len =
        encode_control_frame(CONTROL_PROTOCOL_VERSION, ControlCommandKind::Help as u8, 2, &[], &mut good).unwrap();
    let mut decoder = ControlCommandStreamDecoder::new();
    let mut results = std::vec::Vec::new();
    for &byte in damaged[..damaged_len].iter().chain(&good[..good_len]) {
        if let Some(result) = decoder.push(byte) {
            results.push(result);
        }
    }
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].sequence, 2);
    assert_eq!(results[0].command, Ok(Command::Help));
}

#[test]
fn event_encoder_accepts_the_exact_frame_size() {
    let mut exact = [0; 8];
    assert_eq!(
        encode_control_event(ControlEvent::CommandResult(CommandStatus::Queued), 7, &mut exact).unwrap(),
        exact.len()
    );
    assert_eq!(&exact, &[0x50, 0x44, 1, 0x80, 7, 1, 0, 0x50]);

    let mut short = [0; 7];
    assert_eq!(
        encode_control_event(ControlEvent::CommandResult(CommandStatus::Queued), 7, &mut short),
        Err(ControlFrameError::OutputTooSmall)
    );
}

#[test]
#[cfg(feature = "rich-telemetry")]
fn packet_event_encoder_keeps_protocol_v1_payloads() {
    let capabilities = SourceCapabilities::new(
        CapabilitiesKind::Epr,
        &[
            0x0a91_912c,
            0x0012_d12c,
            0x0013_c12c,
            0x0014_b12c,
            0x0016_41f4,
            0xc9a4_3264,
            0,
            0x0018_c1f4,
            0x001b_41f4,
            0x001f_01f4,
            0xd7c0_96f0,
        ],
    )
    .unwrap();
    let plan = RequestPlanner::new()
        .for_pdo(
            &capabilities,
            PortMode::Epr,
            10,
            Demand::Maximum,
            RequestContext {
                flags: RequestFlags { epr_capable: true, ..RequestFlags::default() },
                limits: SinkLimits { max_power: Some(Milliwatts(140_000)), ..SinkLimits::default() },
                source_present_pdp: None,
            },
        )
        .unwrap();

    let events: &[(ControlEvent, ControlEventKind, &[u8])] = &[
        (
            ControlEvent::Lifecycle { event: ControlLifecycleEvent::PdStoppedTimeout, detail: 10_000, extra: 0 },
            ControlEventKind::Lifecycle,
            &[8, 0x10, 0x27, 0, 0, 0, 0, 0, 0],
        ),
        (
            ControlEvent::Plan { stage: ControlPlanStage::Contract, plan: Some(plan) },
            ControlEventKind::Plan,
            &[
                2, 1, 10, 0, 1, 0, 0x80, 0xbb, 0, 0, 0x80, 0xbb, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0x88, 0x13, 0, 0,
                0x5e, 0x0b, 0, 0, 0, 4, 0x23, 0x8d, 0x44, 0xa1, 0xf4, 0x01, 0x1f, 0,
            ],
        ),
        (
            ControlEvent::ControllerError(ControllerError::Plan(pd_sink::PlanError::VoltageAboveSinkLimit {
                requested: Millivolts(48_100),
                maximum: Millivolts(48_000),
            })),
            ControlEventKind::ControllerError,
            &[22, 0xe4, 0xbb, 0, 0, 0x80, 0xbb, 0, 0],
        ),
        (
            ControlEvent::ControllerError(ControllerError::EprExitRefused(EprExitRefusal::NoSuitableSprContract)),
            ControlEventKind::ControllerError,
            &[6, 1, 0, 0, 0, 0, 0, 0, 0],
        ),
        (
            ControlEvent::ControllerError(ControllerError::EprEntryRefused(EprEntryRefusal::CapabilitiesChanged)),
            ControlEventKind::ControllerError,
            &[7, 2, 0, 0, 0, 0, 0, 0, 0],
        ),
        (
            ControlEvent::SourceInfo { present_watts: 240, maximum_watts: 240, reported_watts: 1 },
            ControlEventKind::SourceInfo,
            &[240, 240, 1],
        ),
        (ControlEvent::RequestResult(RequestResult::Deferred), ControlEventKind::RequestResult, &[1]),
        (
            ControlEvent::Epr { event: ControlEprEvent::DiscoveryStarted, detail: 1, extra: 2 },
            ControlEventKind::Epr,
            &[1, 1, 2],
        ),
        (ControlEvent::CapabilityPlansStarted { count: 11 }, ControlEventKind::CapabilityPlansStarted, &[11]),
        (
            ControlEvent::CapabilityPlanUnavailable { position: 7, validity: PdoValidity::Compatible },
            ControlEventKind::CapabilityPlanUnavailable,
            &[7, 1],
        ),
        (
            ControlEvent::ContractTransition(ContractTransition {
                kind: ContractTransitionKind::SameVoltageSufficientCurrent,
                from: Some(ContractOperatingPoint { voltage: Millivolts(5_000), current: Milliamps(2_000) }),
                to: ContractOperatingPoint { voltage: Millivolts(5_000), current: Milliamps(3_000) },
            }),
            ControlEventKind::ContractTransition,
            &[2, 0x88, 0x13, 0, 0, 0xd0, 0x07, 0, 0, 0x88, 0x13, 0, 0, 0xb8, 0x0b, 0, 0],
        ),
        (
            ControlEvent::IntegrationError(ControlIntegrationError::CapabilityPlansUnavailable),
            ControlEventKind::IntegrationError,
            &[2],
        ),
        (ControlEvent::Help, ControlEventKind::Help, &[]),
    ];

    for &(event, expected_kind, expected_payload) in events {
        let mut packet = [0; CONTROL_MAX_FRAME_LEN];
        let packet_len = encode_control_event_packet(event, 33, &mut packet);
        let frame = decode_one(&packet[..packet_len]);
        assert_eq!(frame.kind, expected_kind as u8);
        assert_eq!(frame.sequence, 33);
        assert_eq!(frame.payload(), expected_payload);

        let mut slice = [0; CONTROL_MAX_FRAME_LEN];
        let slice_len = encode_control_event(event, 33, &mut slice).unwrap();
        assert_eq!(&slice[..slice_len], &packet[..packet_len]);
    }
}

#[test]
#[cfg(not(feature = "rich-telemetry"))]
fn rich_telemetry_commands_are_explicitly_unsupported() {
    for kind in [
        ControlCommandKind::Capabilities,
        ControlCommandKind::Plans,
        ControlCommandKind::SourceInfo,
        ControlCommandKind::SourceStatus,
        ControlCommandKind::PpsStatus,
    ] {
        let mut bytes = [0; CONTROL_MAX_FRAME_LEN];
        let len = encode_control_frame(CONTROL_PROTOCOL_VERSION, kind as u8, 1, &[], &mut bytes).unwrap();
        assert_eq!(decode_control_command(&decode_one(&bytes[..len])), Err(ControlCommandDecodeError::UnknownCommand));
        assert_eq!(decode_stream_command(&bytes[..len]).command, Err(ControlCommandDecodeError::UnknownCommand));
    }
}
