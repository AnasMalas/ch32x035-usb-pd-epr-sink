use std::collections::VecDeque;
use std::future::{Future, pending};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use usbpd::protocol_layer::message::Message;
use usbpd::protocol_layer::message::header::{
    ControlMessageType, DataMessageType, Header, MessageType, SpecificationRevision,
};
use usbpd::sink::device_policy_manager::DevicePolicyManager;
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

struct NeverTimer;

impl Timer for NeverTimer {
    async fn after_millis(_milliseconds: u64) {
        pending().await
    }
}

fn source_header(message_id: u8, message_type: MessageType, num_objects: u8) -> Header {
    let raw_type = match message_type {
        MessageType::Control(message_type) => message_type as u8,
        MessageType::Data(message_type) => message_type as u8,
        MessageType::Extended(message_type) => message_type as u8,
    };
    Header::new_template(DataRole::Dfp, PowerRole::Source, SpecificationRevision::R3_X)
        .with_message_id(message_id)
        .with_message_type_raw(raw_type)
        .with_num_objects(num_objects)
}

fn source_control(message_id: u8, message_type: ControlMessageType) -> Vec<u8> {
    let mut bytes = vec![0; 2];
    source_header(message_id, MessageType::Control(message_type), 0).to_bytes(&mut bytes);
    bytes
}

fn source_capabilities(message_id: u8, pdos: &[u32]) -> Vec<u8> {
    let mut bytes = vec![0; 2 + pdos.len() * 4];
    source_header(message_id, MessageType::Data(DataMessageType::SourceCapabilities), pdos.len() as u8)
        .to_bytes(&mut bytes[..2]);
    for (destination, pdo) in bytes[2..].chunks_exact_mut(4).zip(pdos) {
        destination.copy_from_slice(&pdo.to_le_bytes());
    }
    bytes
}

fn fixed_pdo(voltage_mv: u32, current_ma: u32) -> u32 {
    ((voltage_mv / 50) << 10) | (current_ma / 10)
}

struct SoftResetDriver {
    receive: VecDeque<(usize, Vec<u8>)>,
    transmitted: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Driver for SoftResetDriver {
    async fn wait_for_vbus(&mut self) {}

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        let transmitted = self.transmitted.lock().unwrap().len();
        let Some((required_transmits, _)) = self.receive.front() else {
            return Err(DriverRxError::Detached);
        };
        if transmitted < *required_transmits {
            pending().await
        }

        let (_, message) = self.receive.pop_front().unwrap();
        buffer[..message.len()].copy_from_slice(&message);
        Ok(message.len())
    }

    async fn transmit(&mut self, data: &[u8]) -> Result<(), DriverTxError> {
        self.transmitted.lock().unwrap().push(data.to_vec());
        Ok(())
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        panic!("a valid partner-initiated Soft Reset must not escalate to Hard Reset")
    }
}

struct SoftResetDpm {
    detached: Arc<AtomicBool>,
}

impl DevicePolicyManager for SoftResetDpm {
    async fn detached(&mut self) {
        self.detached.store(true, Ordering::SeqCst);
    }
}

#[test]
fn partner_soft_reset_is_acknowledged_before_accept_with_reset_message_id() {
    let fixed_5v = fixed_pdo(5_000, 3_000);
    let receive = VecDeque::from([
        (0, source_capabilities(0, &[fixed_5v])),
        (2, source_control(0, ControlMessageType::GoodCRC)),
        (2, source_control(1, ControlMessageType::Accept)),
        (3, source_control(2, ControlMessageType::PsRdy)),
        (4, source_control(3, ControlMessageType::SoftReset)),
        (6, source_control(0, ControlMessageType::GoodCRC)),
    ]);

    let transmitted = Arc::new(Mutex::new(Vec::new()));
    let detached = Arc::new(AtomicBool::new(false));
    let driver = SoftResetDriver { receive, transmitted: Arc::clone(&transmitted) };
    let dpm = SoftResetDpm { detached: Arc::clone(&detached) };
    let mut sink: Sink<_, NeverTimer, _> = Sink::new(driver, dpm);

    assert!(matches!(block_on(sink.run()), Err(SinkError::Detached)));
    assert!(detached.load(Ordering::SeqCst));

    let transmitted = transmitted.lock().unwrap();
    assert_eq!(transmitted.len(), 6);

    let soft_reset_good_crc = Message::from_bytes(&transmitted[4]).unwrap();
    assert_eq!(soft_reset_good_crc.header.message_type(), MessageType::Control(ControlMessageType::GoodCRC));
    assert_eq!(soft_reset_good_crc.header.message_id(), 3);

    let accept = Message::from_bytes(&transmitted[5]).unwrap();
    assert_eq!(accept.header.message_type(), MessageType::Control(ControlMessageType::Accept));
    assert_eq!(accept.header.message_id(), 0);
}
