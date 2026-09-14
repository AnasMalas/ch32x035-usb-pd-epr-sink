#![cfg(not(feature = "source"))]

use usbpd::protocol_layer::message::data::Data;
use usbpd::protocol_layer::message::data::source_capabilities::SourceCapabilities;
use usbpd::protocol_layer::message::{Message, Payload};

const CAPTURED_EPR_REQUEST: &[u8] = &[0x89, 0x28, 0xF4, 0xD1, 0xC7, 0x80, 0xF4, 0xC1, 0x18, 0x00];

#[test]
fn source_only_request_payload_is_not_decoded() {
    let message = Message::from_bytes(CAPTURED_EPR_REQUEST).expect("the frame itself remains valid");
    assert!(matches!(message.payload, Some(Payload::Data(Data::Unknown))));
}

#[test]
fn source_only_payload_cannot_be_serialized_accidentally() {
    let source_message = Data::SourceCapabilities(SourceCapabilities::new_vsafe5v_only(300));
    let result = std::panic::catch_unwind(|| {
        let mut payload = [0; 4];
        source_message.to_bytes(&mut payload)
    });
    assert!(result.is_err());
}
