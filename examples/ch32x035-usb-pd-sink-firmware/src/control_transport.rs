use ch32_hal::usb_x0fs::cdc::{Receiver as CdcReceiver, Sender as CdcSender};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use pd_sink::{
    encode_control_event_packet, Command, CommandStatus, ControlCommandDecodeError, ControlCommandStreamDecoder,
    ControlEvent, ControlLifecycleEvent, CONTROL_MAX_FRAME_LEN,
};

use crate::{device_info, set_user_output_enabled, COMMANDS};

const CONTROL_QUEUE_DEPTH: usize = 16;
const USB_CDC_PACKET_LEN: usize = 64;
const _: () = assert!(CONTROL_MAX_FRAME_LEN <= USB_CDC_PACKET_LEN);

#[derive(Clone, Copy)]
struct ControlPacket {
    bytes: [u8; CONTROL_MAX_FRAME_LEN],
    len: u8,
}

impl ControlPacket {
    fn encode(event: ControlEvent, sequence: u8) -> Self {
        let mut bytes = [0; CONTROL_MAX_FRAME_LEN];
        let len = encode_control_event_packet(event, sequence, &mut bytes);
        Self { bytes, len: len as u8 }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    #[cfg(feature = "persistent-black-box")]
    fn raw(bytes: &[u8]) -> Self {
        let mut packet = [0; CONTROL_MAX_FRAME_LEN];
        packet[..bytes.len()].copy_from_slice(bytes);
        Self { bytes: packet, len: bytes.len() as u8 }
    }
}

static CONTROL_PACKETS: Channel<CriticalSectionRawMutex, ControlPacket, CONTROL_QUEUE_DEPTH> = Channel::new();

#[derive(Clone, Copy)]
enum CommandFollowUp {
    None,
    Device,
    Help,
}

pub fn try_emit(event: ControlEvent) {
    try_emit_sequence(event, 0);
}

pub fn try_emit_sequence(event: ControlEvent, sequence: u8) {
    let _ = CONTROL_PACKETS.try_send(ControlPacket::encode(event, sequence));
}

async fn emit(event: ControlEvent, sequence: u8) {
    CONTROL_PACKETS.send(ControlPacket::encode(event, sequence)).await;
}

fn clear_packets() {
    while CONTROL_PACKETS.try_receive().is_ok() {}
}

pub async fn receive(mut receiver: CdcReceiver<'static>) -> ! {
    let mut packet = [0u8; 64];
    let mut decoder = ControlCommandStreamDecoder::new();

    loop {
        receiver.wait_connection().await;
        decoder.reset();
        clear_packets();
        emit(ControlEvent::Lifecycle { event: ControlLifecycleEvent::UsbReady, detail: 0, extra: 0 }, 0).await;
        emit(ControlEvent::Device(device_info()), 0).await;

        loop {
            let count = match receiver.read_packet(&mut packet).await {
                Ok(count) => count,
                Err(_) => break,
            };

            #[cfg(feature = "persistent-black-box")]
            if count == 3 && packet[..2] == crate::black_box::REQUEST_MAGIC {
                decoder.reset();
                let page = packet[2];
                if page == crate::black_box::ARM_REQUEST_PAGE {
                    crate::black_box::arm();
                }
                let response =
                    crate::black_box::response(if page == crate::black_box::ARM_REQUEST_PAGE { 0 } else { page });
                CONTROL_PACKETS.send(ControlPacket::raw(response.as_bytes())).await;
                continue;
            }

            for &byte in &packet[..count] {
                let Some(decoded) = decoder.push(byte) else {
                    continue;
                };
                let sequence = decoded.sequence;
                let command = match decoded.command {
                    Ok(command) => command,
                    Err(error) => {
                        let status = match error {
                            ControlCommandDecodeError::UnsupportedVersion
                            | ControlCommandDecodeError::UnknownCommand => CommandStatus::Unsupported,
                            ControlCommandDecodeError::InvalidLength | ControlCommandDecodeError::InvalidValue => {
                                CommandStatus::Invalid
                            }
                        };
                        emit(ControlEvent::CommandResult(status), sequence).await;
                        continue;
                    }
                };

                let (status, follow_up) = match command {
                    Command::OutputOn => {
                        set_user_output_enabled(true);
                        (CommandStatus::Queued, CommandFollowUp::None)
                    }
                    Command::OutputOff => {
                        set_user_output_enabled(false);
                        (CommandStatus::Queued, CommandFollowUp::None)
                    }
                    Command::Identity => (CommandStatus::Queued, CommandFollowUp::Device),
                    Command::Help => (CommandStatus::Queued, CommandFollowUp::Help),
                    command => {
                        let status = if COMMANDS.try_send(command).is_ok() {
                            CommandStatus::Queued
                        } else {
                            CommandStatus::Busy
                        };
                        (status, CommandFollowUp::None)
                    }
                };

                emit(ControlEvent::CommandResult(status), sequence).await;
                let follow_up = match follow_up {
                    CommandFollowUp::None => None,
                    CommandFollowUp::Device => Some(ControlEvent::Device(device_info())),
                    CommandFollowUp::Help => Some(ControlEvent::Help),
                };
                if let Some(event) = follow_up {
                    emit(event, sequence).await;
                }
            }
        }
    }
}

pub async fn transmit(mut sender: CdcSender<'static>) -> ! {
    'connection: loop {
        sender.wait_connection().await;
        loop {
            let packet = CONTROL_PACKETS.receive().await;
            if sender.write_packet(packet.as_bytes()).await.is_err() {
                continue 'connection;
            }
        }
    }
}
