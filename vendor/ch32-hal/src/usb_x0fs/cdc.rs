//! Compact CDC-ACM implementation for CH32X035.
//!
//! This is intentionally a single-function USB device rather than a generic
//! USB class framework. Control transfers and endpoint state are serviced in
//! the USBFS interrupt, while [`Sender`] and [`Receiver`] expose small async
//! packet APIs to application code. The descriptor/endpoint layout follows
//! the USB CDC specification and WCH's CH32X035 USBFS CDC example.

use core::future::poll_fn;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use core::task::Poll;

use embassy_sync::waitqueue::AtomicWaker;

use super::connection::ConnectionState;
use super::{
    configure_usb_pins, endpoint_ctrl, endpoint_dma, endpoint_t_len, regs, RESPONSE_ACK, RESPONSE_NAK, RESPONSE_STALL,
    TOKEN_IN, TOKEN_OUT, TOKEN_SETUP,
};
use crate::gpio::{Pull, SealedPin};
use crate::interrupt::typelevel::Interrupt;
use crate::peripheral::SealedRccPeripheral;
use crate::{interrupt, peripherals, Peri};

const EP_SIZE: usize = 64;

const STAGE_IDLE: u8 = 0;
const STAGE_DATA_IN: u8 = 1;
const STAGE_STATUS_OUT: u8 = 2;
const STAGE_DATA_OUT: u8 = 3;
const STAGE_STATUS_IN: u8 = 4;

const ACTION_NONE: u8 = 0;
const ACTION_SET_ADDRESS: u8 = 1;
const ACTION_SET_CONFIGURATION: u8 = 2;

const REQUEST_TYPE_STANDARD: u8 = 0x00;
const REQUEST_TYPE_CLASS: u8 = 0x20;
const REQUEST_TYPE_MASK: u8 = 0x60;
const RECIPIENT_MASK: u8 = 0x1f;
const RECIPIENT_DEVICE: u8 = 0x00;
const RECIPIENT_INTERFACE: u8 = 0x01;
const RECIPIENT_ENDPOINT: u8 = 0x02;

const GET_STATUS: u8 = 0x00;
const CLEAR_FEATURE: u8 = 0x01;
const SET_FEATURE: u8 = 0x03;
const SET_ADDRESS: u8 = 0x05;
const GET_DESCRIPTOR: u8 = 0x06;
const GET_CONFIGURATION: u8 = 0x08;
const SET_CONFIGURATION: u8 = 0x09;
const GET_INTERFACE: u8 = 0x0a;
const SET_INTERFACE: u8 = 0x0b;

const CDC_SET_LINE_CODING: u8 = 0x20;
const CDC_GET_LINE_CODING: u8 = 0x21;
const CDC_SET_CONTROL_LINE_STATE: u8 = 0x22;

const DESCRIPTOR_DEVICE: u8 = 1;
const DESCRIPTOR_CONFIGURATION: u8 = 2;
const DESCRIPTOR_STRING: u8 = 3;

// Development VID/PID from WCH's X035 CDC example. Product firmware must use
// assigned identifiers before distribution.
const DEVICE_DESCRIPTOR: [u8; 18] = [
    18, 1, 0x00, 0x02, 0x02, 0x00, 0x00, 64, 0x86, 0x1a, 0x0c, 0xfe, 0x00, 0x01, 1, 2, 3, 1,
];

const CONFIGURATION_DESCRIPTOR: [u8; 67] = [
    // Configuration: two interfaces, bus powered, 100 mA maximum.
    9, 2, 67, 0, 2, 1, 0, 0x80, 50, // CDC communication interface.
    9, 4, 0, 0, 1, 0x02, 0x02, 0x01, 0, // Header, call management, ACM, and union functional descriptors.
    5, 0x24, 0x00, 0x10, 0x01, 5, 0x24, 0x01, 0x00, 0x01, 4, 0x24, 0x02, 0x02, 5, 0x24, 0x06, 0x00, 0x01,
    // Notification endpoint 1 IN (not used by the console data path).
    7, 5, 0x81, 0x03, 8, 0, 0xff, // CDC data interface.
    9, 4, 1, 0, 2, 0x0a, 0x00, 0x00, 0, // Endpoint 2 OUT and endpoint 3 IN.
    7, 5, 0x02, 0x02, 64, 0, 0, 7, 5, 0x83, 0x02, 64, 0, 0,
];

const STRING_LANGUAGE: [u8; 4] = [4, 3, 0x09, 0x04];
const STRING_MANUFACTURER: [u8; 20] = [
    20, 3, b'P', 0, b'r', 0, b'o', 0, b't', 0, b'o', 0, b't', 0, b'y', 0, b'p', 0, b'e', 0,
];
const STRING_PRODUCT: [u8; 32] = [
    32, 3, b'P', 0, b'D', 0, b' ', 0, b'S', 0, b'i', 0, b'n', 0, b'k', 0, b' ', 0, b'C', 0, b'o', 0, b'n', 0, b's', 0,
    b'o', 0, b'l', 0, b'e', 0,
];
const STRING_SERIAL: [u8; 18] = [
    18, 3, b'D', 0, b'E', 0, b'V', 0, b'-', 0, b'0', 0, b'0', 0, b'0', 0, b'1', 0,
];
const LINE_CODING: [u8; 7] = [0x00, 0xc2, 0x01, 0x00, 0, 0, 8];

#[repr(C, align(4))]
struct DmaBuffer([u8; EP_SIZE]);

static mut EP0_BUFFER: DmaBuffer = DmaBuffer([0; EP_SIZE]);
static mut EP1_BUFFER: DmaBuffer = DmaBuffer([0; EP_SIZE]);
static mut EP2_BUFFER: DmaBuffer = DmaBuffer([0; EP_SIZE]);
static mut EP3_BUFFER: DmaBuffer = DmaBuffer([0; EP_SIZE]);

struct ControlState {
    data: *const u8,
    remaining: u16,
    stage: u8,
    action: u8,
    action_value: u8,
    configuration: u8,
}

static mut CONTROL: ControlState = ControlState {
    data: core::ptr::null(),
    remaining: 0,
    stage: STAGE_IDLE,
    action: ACTION_NONE,
    action_value: 0,
    configuration: 0,
};

static RX_WAKER: AtomicWaker = AtomicWaker::new();
static TX_WAKER: AtomicWaker = AtomicWaker::new();

static CONNECTION: ConnectionState = ConnectionState::new();
static RX_READY: AtomicBool = AtomicBool::new(false);
static RX_LENGTH: AtomicU8 = AtomicU8::new(0);
static TX_BUSY: AtomicBool = AtomicBool::new(false);
static CONNECTION_GENERATION: AtomicU8 = AtomicU8::new(0);

fn advance_connection_generation() {
    // Called only by the USB interrupt handler on this single-core target.
    let next = CONNECTION_GENERATION.load(Ordering::SeqCst).wrapping_add(1);
    CONNECTION_GENERATION.store(next, Ordering::SeqCst);
}

#[derive(Clone, Copy)]
struct SetupPacket {
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Disconnected,
    PacketTooLarge,
}

#[inline]
fn ep0_ptr() -> *mut u8 {
    // Raw access is synchronized by USB endpoint ownership and critical
    // sections in the public packet APIs.
    unsafe { core::ptr::addr_of_mut!(EP0_BUFFER.0).cast::<u8>() }
}

#[inline]
fn ep1_ptr() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(EP1_BUFFER.0).cast::<u8>() }
}

#[inline]
fn ep2_ptr() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(EP2_BUFFER.0).cast::<u8>() }
}

#[inline]
fn ep3_ptr() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(EP3_BUFFER.0).cast::<u8>() }
}

unsafe fn copy_to_dma(destination: *mut u8, source: *const u8, count: usize) {
    for index in 0..count {
        core::ptr::write_volatile(destination.add(index), core::ptr::read(source.add(index)));
    }
}

unsafe fn copy_from_dma(destination: *mut u8, source: *const u8, count: usize) {
    for index in 0..count {
        core::ptr::write(destination.add(index), core::ptr::read_volatile(source.add(index)));
    }
}

unsafe fn read_setup() -> SetupPacket {
    let p = ep0_ptr();
    SetupPacket {
        request_type: core::ptr::read_volatile(p),
        request: core::ptr::read_volatile(p.add(1)),
        value: u16::from_le_bytes([core::ptr::read_volatile(p.add(2)), core::ptr::read_volatile(p.add(3))]),
        index: u16::from_le_bytes([core::ptr::read_volatile(p.add(4)), core::ptr::read_volatile(p.add(5))]),
        length: u16::from_le_bytes([core::ptr::read_volatile(p.add(6)), core::ptr::read_volatile(p.add(7))]),
    }
}

unsafe fn control_state() -> &'static mut ControlState {
    &mut *core::ptr::addr_of_mut!(CONTROL)
}

unsafe fn send_control_chunk(first: bool) {
    let state = control_state();
    let count = usize::from(state.remaining.min(EP_SIZE as u16));
    copy_to_dma(ep0_ptr(), state.data, count);
    state.data = state.data.add(count);
    state.remaining -= count as u16;
    endpoint_t_len(0).write(|w| w.set_t_len(count as u8));
    endpoint_ctrl(0).modify(|w| {
        w.set_t_tog(if first { true } else { !w.t_tog() });
        w.set_t_res(RESPONSE_ACK);
        w.set_r_res(RESPONSE_NAK);
    });
}

unsafe fn start_control_in(data: &[u8], requested: u16) {
    let count = data.len().min(usize::from(requested));
    let state = control_state();
    state.data = data.as_ptr();
    state.remaining = count as u16;
    state.stage = STAGE_DATA_IN;
    state.action = ACTION_NONE;
    send_control_chunk(true);
}

unsafe fn start_status_in(action: u8, value: u8) {
    let state = control_state();
    state.stage = STAGE_STATUS_IN;
    state.action = action;
    state.action_value = value;
    endpoint_t_len(0).write(|w| w.set_t_len(0));
    endpoint_ctrl(0).modify(|w| {
        w.set_t_tog(true);
        w.set_t_res(RESPONSE_ACK);
        w.set_r_res(RESPONSE_NAK);
    });
}

unsafe fn stall_control() {
    let state = control_state();
    state.stage = STAGE_IDLE;
    state.action = ACTION_NONE;
    endpoint_ctrl(0).write(|w| {
        w.set_t_tog(true);
        w.set_r_tog(true);
        w.set_t_res(RESPONSE_STALL);
        w.set_r_res(RESPONSE_STALL);
    });
}

fn descriptor(value: u16) -> Option<&'static [u8]> {
    match (value >> 8) as u8 {
        DESCRIPTOR_DEVICE => Some(&DEVICE_DESCRIPTOR),
        DESCRIPTOR_CONFIGURATION => Some(&CONFIGURATION_DESCRIPTOR),
        DESCRIPTOR_STRING => match value as u8 {
            0 => Some(&STRING_LANGUAGE),
            1 => Some(&STRING_MANUFACTURER),
            2 => Some(&STRING_PRODUCT),
            3 => Some(&STRING_SERIAL),
            _ => None,
        },
        _ => None,
    }
}

fn endpoint_address_valid(ep: usize, is_in: bool) -> bool {
    matches!((ep, is_in), (1, true) | (2, false) | (3, true))
}

unsafe fn endpoint_stalled(index: u16) -> Option<bool> {
    let ep = usize::from((index & 0x0f) as u8);
    let is_in = index & 0x80 != 0;
    if !endpoint_address_valid(ep, is_in) {
        return None;
    }
    let ctrl = endpoint_ctrl(ep).read();
    Some(if is_in {
        ctrl.t_res() == RESPONSE_STALL
    } else {
        ctrl.r_res() == RESPONSE_STALL
    })
}

unsafe fn set_endpoint_stall(index: u16, stalled: bool) -> bool {
    let ep = usize::from((index & 0x0f) as u8);
    let is_in = index & 0x80 != 0;
    if !endpoint_address_valid(ep, is_in) {
        return false;
    }
    endpoint_ctrl(ep).modify(|w| {
        if is_in {
            w.set_t_res(if stalled { RESPONSE_STALL } else { RESPONSE_NAK });
            if !stalled {
                w.set_t_tog(false);
            }
        } else {
            let ready = CONNECTION.is_configured() && !RX_READY.load(Ordering::SeqCst);
            w.set_r_res(if stalled {
                RESPONSE_STALL
            } else if ready {
                RESPONSE_ACK
            } else {
                RESPONSE_NAK
            });
            if !stalled {
                w.set_r_tog(false);
            }
        }
    });
    true
}

unsafe fn handle_standard_request(setup: SetupPacket) {
    match setup.request {
        GET_DESCRIPTOR if setup.request_type & 0x80 != 0 => match descriptor(setup.value) {
            Some(data) => start_control_in(data, setup.length),
            None => stall_control(),
        },
        SET_ADDRESS if setup.request_type == RECIPIENT_DEVICE && setup.length == 0 && setup.value <= 127 => {
            start_status_in(ACTION_SET_ADDRESS, setup.value as u8)
        }
        GET_CONFIGURATION if setup.request_type & 0x80 != 0 && setup.length != 0 => {
            let value = [control_state().configuration];
            start_control_in(&value, setup.length);
        }
        SET_CONFIGURATION if setup.request_type == RECIPIENT_DEVICE && setup.length == 0 && setup.value <= 1 => {
            start_status_in(ACTION_SET_CONFIGURATION, setup.value as u8)
        }
        GET_STATUS if setup.request_type & 0x80 != 0 && setup.length >= 2 => {
            let status = match setup.request_type & RECIPIENT_MASK {
                RECIPIENT_DEVICE => [0, 0],
                RECIPIENT_INTERFACE => [0, 0],
                RECIPIENT_ENDPOINT => match endpoint_stalled(setup.index) {
                    Some(stalled) => [u8::from(stalled), 0],
                    None => {
                        stall_control();
                        return;
                    }
                },
                _ => {
                    stall_control();
                    return;
                }
            };
            start_control_in(&status, setup.length);
        }
        CLEAR_FEATURE | SET_FEATURE
            if setup.request_type & RECIPIENT_MASK == RECIPIENT_ENDPOINT && setup.value == 0 && setup.length == 0 =>
        {
            if set_endpoint_stall(setup.index, setup.request == SET_FEATURE) {
                start_status_in(ACTION_NONE, 0);
            } else {
                stall_control();
            }
        }
        GET_INTERFACE
            if setup.request_type & 0x80 != 0
                && setup.request_type & RECIPIENT_MASK == RECIPIENT_INTERFACE
                && setup.length != 0 =>
        {
            start_control_in(&[0], setup.length)
        }
        SET_INTERFACE
            if setup.request_type & RECIPIENT_MASK == RECIPIENT_INTERFACE && setup.value == 0 && setup.length == 0 =>
        {
            start_status_in(ACTION_NONE, 0)
        }
        _ => stall_control(),
    }
}

unsafe fn handle_class_request(setup: SetupPacket) {
    if setup.request_type & RECIPIENT_MASK != RECIPIENT_INTERFACE {
        stall_control();
        return;
    }
    match setup.request {
        CDC_SET_LINE_CODING if setup.request_type & 0x80 == 0 && setup.length == 7 => {
            let state = control_state();
            state.stage = STAGE_DATA_OUT;
            state.action = ACTION_NONE;
            endpoint_ctrl(0).modify(|w| {
                w.set_r_tog(true);
                w.set_r_res(RESPONSE_ACK);
                w.set_t_res(RESPONSE_NAK);
            });
        }
        CDC_GET_LINE_CODING if setup.request_type & 0x80 != 0 => start_control_in(&LINE_CODING, setup.length),
        CDC_SET_CONTROL_LINE_STATE if setup.request_type & 0x80 == 0 && setup.length == 0 => {
            start_status_in(ACTION_NONE, 0)
        }
        _ => stall_control(),
    }
}

unsafe fn handle_setup() {
    let setup = read_setup();
    let state = control_state();
    state.stage = STAGE_IDLE;
    state.action = ACTION_NONE;
    endpoint_ctrl(0).write(|w| {
        w.set_t_tog(true);
        w.set_r_tog(true);
        w.set_t_res(RESPONSE_NAK);
        w.set_r_res(RESPONSE_NAK);
    });

    match setup.request_type & REQUEST_TYPE_MASK {
        REQUEST_TYPE_STANDARD => handle_standard_request(setup),
        REQUEST_TYPE_CLASS => handle_class_request(setup),
        _ => stall_control(),
    }
}

unsafe fn handle_ep0_in() {
    let state = control_state();
    match state.stage {
        STAGE_DATA_IN if state.remaining != 0 => send_control_chunk(false),
        STAGE_DATA_IN => {
            state.stage = STAGE_STATUS_OUT;
            endpoint_ctrl(0).modify(|w| {
                w.set_t_res(RESPONSE_NAK);
                w.set_r_tog(true);
                w.set_r_res(RESPONSE_ACK);
            });
        }
        STAGE_STATUS_IN => {
            match state.action {
                ACTION_SET_ADDRESS => regs().dev_ad().write(|w| w.set_mask_usb_addr(state.action_value)),
                ACTION_SET_CONFIGURATION => {
                    state.configuration = state.action_value;
                    let configured = state.action_value != 0;
                    advance_connection_generation();
                    RX_READY.store(false, Ordering::SeqCst);
                    RX_LENGTH.store(0, Ordering::SeqCst);
                    TX_BUSY.store(false, Ordering::SeqCst);
                    endpoint_ctrl(1).write(|w| w.set_t_res(RESPONSE_NAK));
                    endpoint_ctrl(2).write(|w| w.set_r_res(if configured { RESPONSE_ACK } else { RESPONSE_NAK }));
                    endpoint_ctrl(3).write(|w| w.set_t_res(RESPONSE_NAK));
                    CONNECTION.publish(configured);
                    RX_WAKER.wake();
                    TX_WAKER.wake();
                }
                _ => {}
            }
            state.stage = STAGE_IDLE;
            state.action = ACTION_NONE;
            endpoint_ctrl(0).modify(|w| {
                w.set_t_res(RESPONSE_NAK);
                w.set_r_tog(false);
                w.set_r_res(RESPONSE_ACK);
            });
        }
        _ => endpoint_ctrl(0).modify(|w| w.set_t_res(RESPONSE_NAK)),
    }
}

unsafe fn handle_ep0_out() {
    let state = control_state();
    match state.stage {
        STAGE_DATA_OUT => start_status_in(ACTION_NONE, 0),
        STAGE_STATUS_OUT => {
            state.stage = STAGE_IDLE;
            endpoint_ctrl(0).modify(|w| w.set_r_res(RESPONSE_ACK));
        }
        _ => endpoint_ctrl(0).modify(|w| w.set_r_res(RESPONSE_ACK)),
    }
}

unsafe fn reset_device() {
    let state = control_state();
    state.data = core::ptr::null();
    state.remaining = 0;
    state.stage = STAGE_IDLE;
    state.action = ACTION_NONE;
    state.action_value = 0;
    state.configuration = 0;

    advance_connection_generation();
    RX_READY.store(false, Ordering::SeqCst);
    RX_LENGTH.store(0, Ordering::SeqCst);
    TX_BUSY.store(false, Ordering::SeqCst);

    regs().dev_ad().write(|w| w.set_mask_usb_addr(0));
    regs().uep4_1_mod().write(|w| w.set_tx_en(1, true));
    regs().uep2_3_mod().write(|w| {
        w.set_rx_en(0, true);
        w.set_tx_en(1, true);
    });
    endpoint_ctrl(0).write(|w| {
        w.set_r_res(RESPONSE_ACK);
        w.set_t_res(RESPONSE_NAK);
    });
    endpoint_ctrl(1).write(|w| w.set_t_res(RESPONSE_NAK));
    endpoint_ctrl(2).write(|w| w.set_r_res(RESPONSE_NAK));
    endpoint_ctrl(3).write(|w| w.set_t_res(RESPONSE_NAK));
    endpoint_t_len(1).write(|w| w.set_t_len(0));
    endpoint_t_len(3).write(|w| w.set_t_len(0));

    CONNECTION.publish(false);
    RX_WAKER.wake();
    TX_WAKER.wake();
}

/// Interrupt handler for the compact CDC device.
pub struct InterruptHandler;

impl interrupt::typelevel::Handler<interrupt::typelevel::USBFS> for InterruptHandler {
    unsafe fn on_interrupt() {
        let regs = regs();
        let flags = regs.int_fg().read();

        if flags.bus_rst() {
            reset_device();
            regs.int_fg().write(|w| w.set_bus_rst(true));
            if flags.transfer() {
                // A transfer status sampled with reset is no longer meaningful.
                regs.int_fg().write(|w| w.set_transfer(true));
            }
        } else if flags.transfer() {
            let status = regs.int_st().read();
            let ep = status.mask_uis_endp();
            match (status.mask_token(), ep) {
                (TOKEN_SETUP, _) => handle_setup(),
                (TOKEN_IN, 0) => handle_ep0_in(),
                (TOKEN_OUT, 0) => handle_ep0_out(),
                (TOKEN_IN, 1) => {
                    endpoint_ctrl(1).modify(|w| {
                        w.set_t_res(RESPONSE_NAK);
                        w.set_t_tog(!w.t_tog());
                    });
                }
                (TOKEN_OUT, 2) if status.tog_ok() && !RX_READY.load(Ordering::SeqCst) => {
                    let length = regs.rx_len().read().rx_len().min(EP_SIZE as u16) as u8;
                    RX_LENGTH.store(length, Ordering::SeqCst);
                    RX_READY.store(true, Ordering::SeqCst);
                    endpoint_ctrl(2).modify(|w| {
                        w.set_r_res(RESPONSE_NAK);
                        w.set_r_tog(!w.r_tog());
                    });
                    RX_WAKER.wake();
                }
                (TOKEN_IN, 3) => {
                    endpoint_ctrl(3).modify(|w| {
                        w.set_t_res(RESPONSE_NAK);
                        w.set_t_tog(!w.t_tog());
                    });
                    TX_BUSY.store(false, Ordering::SeqCst);
                    TX_WAKER.wake();
                }
                _ => {}
            }
            regs.int_fg().write(|w| w.set_transfer(true));
        }

        if flags.suspend() {
            regs.int_fg().write(|w| w.set_suspend(true));
        }
        if flags.fifo_ov() {
            regs.int_fg().write(|w| w.set_fifo_ov(true));
        }
    }
}

/// Compact CDC-ACM device handle.
pub struct CdcAcm<'d> {
    _phantom: PhantomData<&'d mut peripherals::USBFS>,
}

impl<'d> CdcAcm<'d> {
    pub fn new(
        _usb: Peri<'d, peripherals::USBFS>,
        dm: Peri<'d, peripherals::PC16>,
        dp: Peri<'d, peripherals::PC17>,
        _irq: impl interrupt::typelevel::Binding<interrupt::typelevel::USBFS, InterruptHandler> + 'd,
    ) -> Self {
        dm.set_as_input(Pull::None);
        dp.set_as_input(Pull::Up);
        peripherals::USBFS::enable_and_reset();
        configure_usb_pins(true);

        let regs = regs();
        regs.ctrl().write(|w| {
            w.set_clr_all(true);
            w.set_reset_sie(true);
        });
        embassy_time::block_for(embassy_time::Duration::from_micros(10));
        regs.ctrl().write(|_| {});

        endpoint_dma(0).write_value(crate::pac::usb::regs::UepDma(ep0_ptr() as u32));
        endpoint_dma(1).write_value(crate::pac::usb::regs::UepDma(ep1_ptr() as u32));
        endpoint_dma(2).write_value(crate::pac::usb::regs::UepDma(ep2_ptr() as u32));
        endpoint_dma(3).write_value(crate::pac::usb::regs::UepDma(ep3_ptr() as u32));
        unsafe { reset_device() };

        regs.int_fg().write_value(crate::pac::usb::regs::UsbIntFg(0xff));
        regs.ctrl().write(|w| {
            w.set_dev_pu_en(true);
            w.set_int_busy(true);
            w.set_dma_en(true);
        });
        regs.udev_ctrl().write(|w| {
            w.set_pd_dis(true);
            w.set_port_en(true);
        });
        regs.int_en().write(|w| {
            w.set_suspend(true);
            w.set_bus_rst(true);
            w.set_transfer(true);
            w.set_fifo_ov(true);
        });

        critical_section::with(|_| {
            interrupt::typelevel::USBFS::unpend();
            unsafe { interrupt::typelevel::USBFS::enable() };
        });

        Self { _phantom: PhantomData }
    }

    pub fn split(self) -> (Sender<'d>, Receiver<'d>) {
        (Sender { _phantom: PhantomData }, Receiver { _phantom: PhantomData })
    }
}

pub struct Sender<'d> {
    _phantom: PhantomData<&'d mut peripherals::USBFS>,
}

impl Sender<'_> {
    pub const fn max_packet_size(&self) -> usize {
        EP_SIZE
    }

    pub async fn wait_connection(&mut self) {
        CONNECTION.wait_sender().await;
    }

    pub async fn write_packet(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.len() > EP_SIZE {
            return Err(Error::PacketTooLarge);
        }
        if !CONNECTION.is_configured() {
            return Err(Error::Disconnected);
        }

        let generation = loop {
            poll_fn(|cx| {
                TX_WAKER.register(cx.waker());
                if !CONNECTION.is_configured() {
                    Poll::Ready(Err(Error::Disconnected))
                } else if TX_BUSY.load(Ordering::SeqCst) {
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(()))
                }
            })
            .await?;

            let started = critical_section::with(|_| {
                if !CONNECTION.is_configured() {
                    return Err(Error::Disconnected);
                }
                if TX_BUSY.load(Ordering::SeqCst) {
                    return Ok(None);
                }

                unsafe { copy_to_dma(ep3_ptr(), data.as_ptr(), data.len()) };
                let generation = CONNECTION_GENERATION.load(Ordering::SeqCst);
                TX_BUSY.store(true, Ordering::SeqCst);
                endpoint_t_len(3).write(|w| w.set_t_len(data.len() as u8));
                endpoint_ctrl(3).modify(|w| w.set_t_res(RESPONSE_ACK));
                Ok(Some(generation))
            })?;

            if let Some(generation) = started {
                break generation;
            }
        };

        poll_fn(|cx| {
            TX_WAKER.register(cx.waker());
            if !CONNECTION.is_configured() || CONNECTION_GENERATION.load(Ordering::SeqCst) != generation {
                Poll::Ready(Err(Error::Disconnected))
            } else if TX_BUSY.load(Ordering::SeqCst) {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        })
        .await
    }
}

pub struct Receiver<'d> {
    _phantom: PhantomData<&'d mut peripherals::USBFS>,
}

impl Receiver<'_> {
    pub const fn max_packet_size(&self) -> usize {
        EP_SIZE
    }

    pub async fn wait_connection(&mut self) {
        CONNECTION.wait_receiver().await;
    }

    pub async fn read_packet(&mut self, data: &mut [u8]) -> Result<usize, Error> {
        if data.len() < EP_SIZE {
            return Err(Error::PacketTooLarge);
        }

        loop {
            poll_fn(|cx| {
                RX_WAKER.register(cx.waker());
                if !CONNECTION.is_configured() {
                    Poll::Ready(Err(Error::Disconnected))
                } else if RX_READY.load(Ordering::SeqCst) {
                    Poll::Ready(Ok(()))
                } else {
                    Poll::Pending
                }
            })
            .await?;

            let packet = critical_section::with(|_| {
                if !CONNECTION.is_configured() {
                    return Err(Error::Disconnected);
                }
                if !RX_READY.load(Ordering::SeqCst) {
                    return Ok(None);
                }

                let length = usize::from(RX_LENGTH.load(Ordering::SeqCst));
                unsafe { copy_from_dma(data.as_mut_ptr(), ep2_ptr(), length) };
                RX_READY.store(false, Ordering::SeqCst);
                endpoint_ctrl(2).modify(|w| w.set_r_res(RESPONSE_ACK));
                Ok(Some(length))
            })?;

            if let Some(length) = packet {
                return Ok(length);
            }
        }
    }
}
