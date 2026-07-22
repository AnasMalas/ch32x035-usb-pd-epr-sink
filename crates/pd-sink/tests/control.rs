use pd_sink::{
    decode_control_command, encode_control_event, encode_control_frame, CapabilitiesKind, Command, CommandStatus,
    ControlCommandKind, ControlEvent, ControlFrameDecoder, ControlFrameError, ControlPlanStage, Demand, DeviceInfo,
    Milliamps, Millivolts, Milliwatts, Preference, SourceCapabilities, UserRequest, CONTROL_MAX_FRAME_LEN,
    CONTROL_PROTOCOL_VERSION,
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
            flags: DeviceInfo::PPS_SUPPORTED | DeviceInfo::EPR_SUPPORTED,
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
