use std::collections::VecDeque;
use std::future::{Future, pending};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use usbpd::protocol_layer::message::data::request::{
    CurrentRequest, EprRequestDataObject, PowerSource, VoltageRequest,
};
use usbpd::protocol_layer::message::data::source_capabilities::parse_raw_pdo;
use usbpd::protocol_layer::message::extended::Extended;
use usbpd::protocol_layer::message::extended::ExtendedHeader;
use usbpd::protocol_layer::message::extended::extended_control::{ExtendedControl, ExtendedControlMessageType};
use usbpd::protocol_layer::message::extended::sink_capabilities_extended::{
    SINK_MODE_AVS_SUPPORTED, SINK_MODE_PPS_SUPPORTED, SINK_MODE_VBUS_POWERED, SinkCapabilitiesExtended,
};
use usbpd::protocol_layer::message::header::{
    ControlMessageType, DataMessageType, ExtendedMessageType, Header, MessageType, SpecificationRevision,
};
use usbpd::protocol_layer::message::{Message, Payload};
use usbpd::sink::device_policy_manager::{DevicePolicyManager, Event};
use usbpd::sink::policy_engine::{Error as SinkError, Sink};
use usbpd::timers::Timer;
use usbpd::{DataRole, PowerRole};
use usbpd_traits::{Driver, DriverRxError, DriverTxError};

fn block_on<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);

    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

static EPR_CLOCK_TICKS: AtomicU32 = AtomicU32::new(0);

struct EprTransitionTimer;

impl Timer for EprTransitionTimer {
    fn now_128ms_ticks() -> u32 {
        EPR_CLOCK_TICKS.load(Ordering::SeqCst)
    }

    async fn after_millis(_milliseconds: u64) {
        pending().await
    }
}

fn source_header(message_id: u8, message_type: MessageType, num_objects: u8) -> Header {
    let (raw_type, extended) = match message_type {
        MessageType::Control(message_type) => (message_type as u8, false),
        MessageType::Data(message_type) => (message_type as u8, false),
        MessageType::Extended(message_type) => (message_type as u8, true),
    };

    Header::new_template(DataRole::Dfp, PowerRole::Source, SpecificationRevision::R3_X)
        .with_message_id(message_id)
        .with_message_type_raw(raw_type)
        .with_num_objects(num_objects)
        .with_extended(extended)
}

fn source_control(message_id: u8, message_type: ControlMessageType) -> Vec<u8> {
    let mut bytes = vec![0; 2];
    source_header(message_id, MessageType::Control(message_type), 0).to_bytes(&mut bytes);
    bytes
}

fn source_data(message_id: u8, message_type: DataMessageType, objects: &[u32]) -> Vec<u8> {
    let mut bytes = vec![0; 2 + objects.len() * 4];
    source_header(message_id, MessageType::Data(message_type), objects.len() as u8).to_bytes(&mut bytes[..2]);
    for (destination, object) in bytes[2..].chunks_exact_mut(4).zip(objects) {
        destination.copy_from_slice(&object.to_le_bytes());
    }
    bytes
}

fn source_extended_control(message_id: u8, message_type: ExtendedControlMessageType) -> Vec<u8> {
    let mut bytes = vec![0; 6];
    source_header(message_id, MessageType::Extended(ExtendedMessageType::ExtendedControl), 1).to_bytes(&mut bytes[..2]);
    ExtendedHeader::new(2).with_chunked(true).to_bytes(&mut bytes[2..4]);
    bytes[4] = u8::from(message_type);
    bytes
}

fn source_epr_capabilities_chunk(message_id: u8, total_size: u16, chunk: u8, payload: &[u8]) -> Vec<u8> {
    assert_eq!((payload.len() + 2) % 4, 0, "extended chunk must end on a data-object boundary");
    let num_objects = ((payload.len() + 2) / 4) as u8;
    let mut bytes = vec![0; 4 + payload.len()];
    source_header(message_id, MessageType::Extended(ExtendedMessageType::EprSourceCapabilities), num_objects)
        .to_bytes(&mut bytes[..2]);
    ExtendedHeader::new(total_size).with_chunked(true).with_chunk_number(chunk).to_bytes(&mut bytes[2..4]);
    bytes[4..].copy_from_slice(payload);
    bytes
}

fn fixed_pdo(voltage_mv: u32, current_ma: u32, epr_capable: bool) -> u32 {
    ((voltage_mv / 50) << 10) | (current_ma / 10) | (u32::from(epr_capable) << 23)
}

#[test]
fn short_epr_control_messages_use_the_capture_compatible_chunked_form() {
    let header = source_header(0, MessageType::Extended(ExtendedMessageType::ExtendedControl), 1);
    let mut message = Message::new(header);
    message.payload = Some(Payload::Extended(Extended::ExtendedControl(
        ExtendedControl::default().with_message_type(ExtendedControlMessageType::EprKeepAlive),
    )));

    let mut bytes = [0_u8; 8];
    let length = message.to_bytes(&mut bytes);
    let extended_header = ExtendedHeader::from_bytes(&bytes[2..4]);

    assert_eq!(length, 6);
    assert!(extended_header.chunked());
    assert!(!extended_header.request_chunk());
    assert_eq!(extended_header.chunk_number(), 0);
    assert_eq!(extended_header.data_size(), 2);
    assert_eq!(&bytes[4..6], &[u8::from(ExtendedControlMessageType::EprKeepAlive), 0]);
}

struct ScriptedDriver {
    receive: VecDeque<(usize, Vec<u8>)>,
    transmitted: Arc<Mutex<Vec<Vec<u8>>>>,
    sink_tx_ok: Arc<AtomicBool>,
    sink_tx_checks: Arc<AtomicUsize>,
    epr_sink_cap_requested: Arc<AtomicBool>,
    sink_cap_ext_requested: Arc<AtomicBool>,
}

impl Driver for ScriptedDriver {
    const HAS_AUTO_GOOD_CRC: bool = true;
    const HAS_AUTO_RETRY: bool = true;

    async fn wait_for_vbus(&mut self) {}

    fn sink_tx_ok(&mut self) -> bool {
        self.sink_tx_checks.fetch_add(1, Ordering::SeqCst);
        self.sink_tx_ok.load(Ordering::SeqCst)
    }

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        let Some((required_transmits, _)) = self.receive.front() else {
            return Err(DriverRxError::Detached);
        };
        if self.transmitted.lock().unwrap().len() < *required_transmits {
            pending().await
        }

        let releases_sink_tx =
            self.receive.front().and_then(|(_, message)| Header::from_bytes(&message[..2]).ok()).is_some_and(
                |header| matches!(header.message_type(), MessageType::Control(ControlMessageType::GetSinkCap)),
            );
        if releases_sink_tx && self.sink_tx_checks.load(Ordering::SeqCst) == 0 {
            // Force the DPM's sink-initiated event to become pending first.
            // The Source then starts its own AMS while Rp is SinkTxNG.
            pending().await
        }

        let (_, message) = self.receive.pop_front().unwrap();
        if releases_sink_tx {
            self.sink_tx_ok.store(true, Ordering::SeqCst);
        }
        if Header::from_bytes(&message[..2]).ok().is_some_and(|header| {
            matches!(header.message_type(), MessageType::Extended(ExtendedMessageType::ExtendedControl))
        }) && message.get(4) == Some(&u8::from(ExtendedControlMessageType::EprGetSinkCap))
        {
            self.epr_sink_cap_requested.store(true, Ordering::SeqCst);
        }
        if Header::from_bytes(&message[..2]).ok().is_some_and(|header| {
            matches!(header.message_type(), MessageType::Control(ControlMessageType::GetSinkCapExtended))
        }) {
            self.sink_cap_ext_requested.store(true, Ordering::SeqCst);
        }
        assert!(message.len() <= buffer.len());
        buffer[..message.len()].copy_from_slice(&message);
        Ok(message.len())
    }

    async fn transmit(&mut self, data: &[u8]) -> Result<(), DriverTxError> {
        self.transmitted.lock().unwrap().push(data.to_vec());
        Ok(())
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        panic!("successful scripted EPR entry must not hard reset")
    }
}

struct EprDpm {
    events_sent: u8,
    epr_capabilities_seen: Arc<AtomicBool>,
    transitions: Arc<AtomicUsize>,
    detached: Arc<AtomicBool>,
    fixed_5v_pdo: u32,
    fixed_48v_pdo: u32,
    epr_avs_pdo: u32,
    epr_sink_cap_requested: Arc<AtomicBool>,
    sink_cap_ext_requested: Arc<AtomicBool>,
}

fn epr_fixed_request(position: u8, pdo: u32) -> PowerSource {
    let rdo = (u32::from(position) << 28) | (1 << 24) | (1 << 22) | (300 << 10) | 300;
    PowerSource::EprRequest(EprRequestDataObject { rdo, pdo: parse_raw_pdo(pdo) })
}

fn epr_avs_request(position: u8, voltage_mv: u32, current_ma: u32, pdo: u32) -> PowerSource {
    let raw_voltage_25mv = voltage_mv / 25;
    assert_eq!(raw_voltage_25mv & 0x3, 0, "AVS voltage must land on a 100 mV boundary");
    let rdo = (u32::from(position) << 28) | (1 << 24) | (1 << 22) | (raw_voltage_25mv << 9) | (current_ma / 50);
    PowerSource::EprRequest(EprRequestDataObject { rdo, pdo: parse_raw_pdo(pdo) })
}

impl DevicePolicyManager for EprDpm {
    fn sink_capabilities_extended(&self) -> SinkCapabilitiesExtended {
        SinkCapabilitiesExtended::new_v1_power_descriptor(
            0,
            0,
            SINK_MODE_VBUS_POWERED | SINK_MODE_PPS_SUPPORTED | SINK_MODE_AVS_SUPPORTED,
            5,
            15,
            100,
            5,
            140,
            140,
        )
    }

    async fn inform(
        &mut self,
        source_capabilities: &usbpd::protocol_layer::message::data::source_capabilities::SourceCapabilities,
    ) {
        if source_capabilities.is_epr_capabilities() {
            self.epr_capabilities_seen.store(true, Ordering::SeqCst);
        }
    }

    async fn request(
        &mut self,
        source_capabilities: &usbpd::protocol_layer::message::data::source_capabilities::SourceCapabilities,
    ) -> PowerSource {
        if source_capabilities.is_epr_capabilities() {
            epr_fixed_request(8, self.fixed_48v_pdo)
        } else {
            PowerSource::new_fixed(CurrentRequest::Highest, VoltageRequest::Safe5V, source_capabilities).unwrap()
        }
    }

    async fn transition_power(&mut self, _accepted: &PowerSource) {
        self.transitions.fetch_add(1, Ordering::SeqCst);
    }

    async fn detached(&mut self) {
        self.detached.store(true, Ordering::SeqCst);
    }

    fn get_event(
        &mut self,
        _source_capabilities: &usbpd::protocol_layer::message::data::source_capabilities::SourceCapabilities,
    ) -> impl Future<Output = Event> {
        let transitions = self.transitions.load(Ordering::SeqCst);
        let event = match (self.events_sent, transitions) {
            (0, 1) => Some(Event::enter_epr_mode_watts(140)),
            (1, 2)
                if self.epr_sink_cap_requested.load(Ordering::SeqCst)
                    && self.sink_cap_ext_requested.load(Ordering::SeqCst) =>
            {
                Some(Event::RequestPower(epr_avs_request(9, 33_700, 5_000, self.epr_avs_pdo)))
            }
            (2, 3) => {
                // Make the keepalive deadline from the preceding 48 V/AVS
                // contract stale while this long high-to-low transition is in
                // flight. PS_RDY must rearm it before Ready is entered.
                EPR_CLOCK_TICKS.store(10, Ordering::SeqCst);
                Some(Event::RequestPower(epr_fixed_request(1, self.fixed_5v_pdo)))
            }
            (3, 4) => Some(Event::ExitEprMode),
            _ => None,
        };
        if event.is_some() {
            self.events_sent += 1;
        }

        async move {
            match event {
                Some(event) => event,
                None => pending().await,
            }
        }
    }
}

#[test]
fn policy_engine_negotiates_fixed_48v_arbitrary_epr_avs_and_a_legal_exit() {
    EPR_CLOCK_TICKS.store(0, Ordering::SeqCst);
    let fixed_5v = fixed_pdo(5_000, 3_000, true);
    let fixed_20v = fixed_pdo(20_000, 5_000, false);
    let fixed_48v = fixed_pdo(48_000, 5_000, false);
    let epr_avs = (0b11 << 30) | (0b01 << 28) | ((48_000 / 100) << 17) | ((15_000 / 100) << 8) | 140;
    let epr_pdos = [fixed_5v, fixed_20v, 0, 0, 0, 0, 0, fixed_48v, epr_avs];
    let epr_payload: Vec<u8> = epr_pdos.iter().flat_map(|pdo| pdo.to_le_bytes()).collect();

    let scripted_receive = VecDeque::from([
        (0, source_data(0, DataMessageType::SourceCapabilities, &[fixed_5v, fixed_20v])),
        (1, source_control(1, ControlMessageType::Accept)),
        (1, source_control(2, ControlMessageType::PsRdy)),
        (1, source_control(3, ControlMessageType::GetSinkCap)),
        (3, source_data(4, DataMessageType::EprMode, &[(0x02_u32 << 24)])),
        (3, source_data(5, DataMessageType::EprMode, &[(0x03_u32 << 24)])),
        (3, source_epr_capabilities_chunk(6, epr_payload.len() as u16, 0, &epr_payload[..26])),
        (4, source_epr_capabilities_chunk(7, epr_payload.len() as u16, 1, &epr_payload[26..])),
        (5, source_control(0, ControlMessageType::Accept)),
        (5, source_control(1, ControlMessageType::PsRdy)),
        (5, source_extended_control(2, ExtendedControlMessageType::EprGetSinkCap)),
        (6, source_control(3, ControlMessageType::GetSinkCapExtended)),
        (8, source_control(4, ControlMessageType::Accept)),
        (8, source_control(5, ControlMessageType::PsRdy)),
        (9, source_control(6, ControlMessageType::Accept)),
        (9, source_control(7, ControlMessageType::PsRdy)),
        (10, source_data(0, DataMessageType::SourceCapabilities, &[fixed_5v, fixed_20v])),
        (11, source_control(1, ControlMessageType::Accept)),
        (11, source_control(2, ControlMessageType::PsRdy)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let epr_capabilities_seen = Arc::new(AtomicBool::new(false));
    let transitions = Arc::new(AtomicUsize::new(0));
    let detached = Arc::new(AtomicBool::new(false));
    let sink_tx_ok = Arc::new(AtomicBool::new(false));
    let sink_tx_checks = Arc::new(AtomicUsize::new(0));
    let epr_sink_cap_requested = Arc::new(AtomicBool::new(false));
    let sink_cap_ext_requested = Arc::new(AtomicBool::new(false));
    let driver = ScriptedDriver {
        receive: scripted_receive,
        transmitted: Arc::clone(&transmitted),
        sink_tx_ok: Arc::clone(&sink_tx_ok),
        sink_tx_checks: Arc::clone(&sink_tx_checks),
        epr_sink_cap_requested: Arc::clone(&epr_sink_cap_requested),
        sink_cap_ext_requested: Arc::clone(&sink_cap_ext_requested),
    };
    let dpm = EprDpm {
        events_sent: 0,
        epr_capabilities_seen: Arc::clone(&epr_capabilities_seen),
        transitions: Arc::clone(&transitions),
        detached: Arc::clone(&detached),
        fixed_5v_pdo: fixed_5v,
        fixed_48v_pdo: fixed_48v,
        epr_avs_pdo: epr_avs,
        epr_sink_cap_requested: Arc::clone(&epr_sink_cap_requested),
        sink_cap_ext_requested: Arc::clone(&sink_cap_ext_requested),
    };
    let mut sink: Sink<_, EprTransitionTimer, _> = Sink::new(driver, dpm);

    let result = block_on(sink.run());

    assert!(matches!(result, Err(SinkError::Detached)));
    assert!(detached.load(Ordering::SeqCst));
    assert!(epr_capabilities_seen.load(Ordering::SeqCst));
    assert_eq!(transitions.load(Ordering::SeqCst), 5);
    assert!(sink_tx_checks.load(Ordering::SeqCst) >= 5);

    let transmitted = transmitted.lock().unwrap();
    let message_types: Vec<MessageType> =
        transmitted.iter().map(|message| Header::from_bytes(&message[..2]).unwrap().message_type()).collect();
    assert_eq!(
        message_types,
        [
            MessageType::Data(DataMessageType::Request),
            MessageType::Data(DataMessageType::SinkCapabilities),
            MessageType::Data(DataMessageType::EprMode),
            MessageType::Extended(ExtendedMessageType::EprSourceCapabilities),
            MessageType::Data(DataMessageType::EprRequest),
            MessageType::Extended(ExtendedMessageType::EprSinkCapabilities),
            MessageType::Extended(ExtendedMessageType::SinkCapabilitiesExtended),
            MessageType::Data(DataMessageType::EprRequest),
            MessageType::Data(DataMessageType::EprRequest),
            MessageType::Data(DataMessageType::EprMode),
            MessageType::Data(DataMessageType::Request),
        ]
    );

    let enter_object = u32::from_le_bytes(transmitted[2][2..6].try_into().unwrap());
    assert_eq!(enter_object >> 24, 0x01);
    assert_eq!((enter_object >> 16) & 0xff, 140);

    let chunk_request = ExtendedHeader::from_bytes(&transmitted[3][2..4]);
    assert!(chunk_request.chunked());
    assert!(chunk_request.request_chunk());
    assert_eq!(chunk_request.chunk_number(), 1);

    assert_eq!(transmitted[4].len(), 10);
    let epr_rdo = u32::from_le_bytes(transmitted[4][2..6].try_into().unwrap());
    let copied_pdo = u32::from_le_bytes(transmitted[4][6..10].try_into().unwrap());
    assert_eq!(epr_rdo >> 28, 8);
    assert_eq!(copied_pdo, fixed_48v);

    let sink_caps_header = Header::from_bytes(&transmitted[5][..2]).unwrap();
    let sink_caps_extended_header = ExtendedHeader::from_bytes(&transmitted[5][2..4]);
    assert_eq!(sink_caps_header.num_objects(), 2);
    assert_eq!(transmitted[5].len(), 10);
    assert!(sink_caps_extended_header.chunked());
    assert!(!sink_caps_extended_header.request_chunk());
    assert_eq!(sink_caps_extended_header.chunk_number(), 0);
    assert_eq!(sink_caps_extended_header.data_size(), 4);
    let sink_pdo = u32::from_le_bytes(transmitted[5][4..8].try_into().unwrap());
    assert_eq!((sink_pdo >> 10) & 0x3ff, 100, "the default sink capability is fixed 5 V");
    assert_eq!(sink_pdo & 0x3ff, 100, "the default test DPM advertises 1 A");
    assert_eq!(&transmitted[5][8..], &[0, 0], "the last data object must be zero padded");

    let sink_cap_ext_header = Header::from_bytes(&transmitted[6][..2]).unwrap();
    let sink_cap_ext_extended_header = ExtendedHeader::from_bytes(&transmitted[6][2..4]);
    assert_eq!(sink_cap_ext_header.num_objects(), 7);
    assert_eq!(transmitted[6].len(), 30);
    assert!(sink_cap_ext_extended_header.chunked());
    assert_eq!(sink_cap_ext_extended_header.data_size(), 24);
    assert_eq!(transmitted[6][14], 1, "SKEDB version must be 1.0");
    assert_eq!(transmitted[6][21], 0b10_0011, "sink must advertise VBUS, PPS, and AVS modes");
    assert_eq!(transmitted[6][26], 140, "EPR Operational PDP must match EPR Mode Enter");
    assert_eq!(&transmitted[6][28..], &[0, 0], "the last data object must be zero padded");

    let avs_rdo = u32::from_le_bytes(transmitted[7][2..6].try_into().unwrap());
    let copied_avs_pdo = u32::from_le_bytes(transmitted[7][6..10].try_into().unwrap());
    assert_eq!(avs_rdo >> 28, 9);
    assert_eq!((avs_rdo >> 9) & 0xfff, 1_348);
    assert_eq!(avs_rdo & 0x7f, 100);
    assert_eq!(copied_avs_pdo, epr_avs);

    let pre_exit_rdo = u32::from_le_bytes(transmitted[8][2..6].try_into().unwrap());
    let pre_exit_pdo = u32::from_le_bytes(transmitted[8][6..10].try_into().unwrap());
    assert_eq!(pre_exit_rdo >> 28, 1);
    assert_eq!(pre_exit_pdo, fixed_5v);

    let exit_object = u32::from_le_bytes(transmitted[9][2..6].try_into().unwrap());
    assert_eq!(exit_object >> 24, 0x05);
}
