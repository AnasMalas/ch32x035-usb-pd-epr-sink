#![no_std]
#![no_main]

use core::cell::Cell;
#[cfg(feature = "usb-console")]
use core::fmt::{self, Write};

use ch32_hal as hal;
use ch32_hal::exti::ExtiInput;
use ch32_hal::gpio::{Level, Output, Pull, Speed};
#[cfg(feature = "usb-console")]
use ch32_hal::usb_x0fs::cdc::{
    CdcAcm, InterruptHandler as UsbFsInterruptHandler, Receiver as CdcReceiver, Sender as CdcSender,
};
use ch32_hal::usbpd::{Error as UsbpdError, InterruptHandler, Sop, UsbPdPhy};
use ch32_hal::{bind_interrupts, peripherals};
use embassy_executor::Spawner;
#[cfg(feature = "usb-console")]
use embassy_futures::join::join3;
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::Timer;
use panic_halt as _;
#[cfg(feature = "epr-capable-hardware")]
use pd_sink::EprState;
#[cfg(not(all(feature = "usb-console", feature = "sdi-log")))]
use pd_sink::PlanError;
use pd_sink::{
    CapabilitiesKind, Command, ContractState, ContractTracker, ControllerAction, ControllerConfig, ControllerError,
    CurrentConfidence, LimitReason, Milliamps, Millivolts, Milliwatts, PdoValidity, PlannedOperating, PlannedVoltage,
    RequestContext, RequestFlags, RequestMessage, RequestPlan, SinkController, SinkLimits,
    SourceCapabilities as ProductSourceCapabilities, SourceSupply, SupplyKind,
};
#[cfg(any(feature = "bench-pps-19v4", feature = "bench-epr-avs-19v4", feature = "bench-epr-fixed-48v"))]
use pd_sink::{Preference, UserRequest};
use usbpd::protocol_layer::message::data::epr_mode::DataEnterFailed;
use usbpd::protocol_layer::message::data::request::{self, EprRequestDataObject, PowerSource};
use usbpd::protocol_layer::message::data::sink_capabilities::SinkCapabilities;
use usbpd::protocol_layer::message::data::source_capabilities::{
    parse_raw_pdo, Augmented, PowerDataObject, SourceCapabilities,
};
use usbpd::protocol_layer::message::data::source_info::SourceInfo;
use usbpd::protocol_layer::message::extended::sink_capabilities_extended::{
    SinkCapabilitiesExtended, SINK_MODE_AVS_SUPPORTED, SINK_MODE_PPS_SUPPORTED, SINK_MODE_VBUS_POWERED,
};
use usbpd::sink::device_policy_manager::{DevicePolicyManager, Event, HardResetOrigin, RequestRejection};
use usbpd::sink::policy_engine::Sink;
use usbpd::timers::Timer as SinkTimer;
use usbpd_traits::Driver as SinkDriver;

#[cfg(any(
    all(feature = "bench-pps-19v4", feature = "bench-epr-avs-19v4"),
    all(feature = "bench-pps-19v4", feature = "bench-epr-fixed-48v"),
    all(feature = "bench-epr-avs-19v4", feature = "bench-epr-fixed-48v"),
))]
compile_error!("select at most one bench request feature");

static COMMANDS: Channel<CriticalSectionRawMutex, Command, 4> = Channel::new();

#[cfg(feature = "usb-console")]
const CONSOLE_LINE_CAPACITY: usize = 128;
#[cfg(feature = "usb-console")]
const CONSOLE_QUEUE_DEPTH: usize = 32;

#[cfg(feature = "usb-console")]
#[derive(Clone, Copy)]
struct ConsoleLine {
    bytes: [u8; CONSOLE_LINE_CAPACITY],
    len: u8,
}

#[cfg(feature = "usb-console")]
impl ConsoleLine {
    const fn new() -> Self {
        Self { bytes: [0; CONSOLE_LINE_CAPACITY], len: 0 }
    }

    fn finish(&mut self) {
        let mut len = usize::from(self.len).min(CONSOLE_LINE_CAPACITY.saturating_sub(2));
        self.bytes[len] = b'\r';
        len += 1;
        self.bytes[len] = b'\n';
        len += 1;
        self.len = len as u8;
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

#[cfg(feature = "usb-console")]
impl fmt::Write for ConsoleLine {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let start = usize::from(self.len);
        let available = CONSOLE_LINE_CAPACITY.saturating_sub(2).saturating_sub(start);
        let count = value.len().min(available);
        self.bytes[start..start + count].copy_from_slice(&value.as_bytes()[..count]);
        self.len = (start + count) as u8;
        Ok(())
    }
}

#[cfg(feature = "usb-console")]
static CONSOLE_LINES: Channel<CriticalSectionRawMutex, ConsoleLine, CONSOLE_QUEUE_DEPTH> = Channel::new();

#[cfg(all(feature = "usb-console", not(feature = "sdi-log")))]
fn enqueue_console_log(arguments: fmt::Arguments<'_>) {
    let mut line = ConsoleLine::new();
    let _ = line.write_fmt(arguments);
    line.finish();
    let _ = CONSOLE_LINES.try_send(line);
}

#[cfg(all(feature = "usb-console", feature = "sdi-log"))]
fn enqueue_dual_log(arguments: fmt::Arguments<'_>) {
    let mut line = ConsoleLine::new();
    let _ = line.write_fmt(arguments);
    line.finish();

    // ConsoleLine is assembled only from fmt::Write UTF-8 strings plus CRLF,
    // so this conversion cannot create invalid text. Formatting once avoids
    // duplicating every formatter in the flash-constrained dual-log image.
    let text = unsafe { core::str::from_utf8_unchecked(line.as_bytes()) };
    let _ = hal::debug::SDIPrint.write_str(text);
    let _ = CONSOLE_LINES.try_send(line);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadCommand {
    Disable,
    Enable,
}

const ATTACH_DEBOUNCE_MS: u64 = 100;
const HARD_RESET_RECOVERY_MS: u64 = 2_000;
const PROTOCOL_RESTART_COOLDOWN_MS: u64 = 2_000;
const MAX_AUTO_EPR_ATTEMPTS: u8 = 2;

static VBUS_PRESENT: Mutex<CriticalSectionRawMutex, Cell<bool>> = Mutex::new(Cell::new(false));
static VBUS_ATTACHED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static VBUS_DETACHED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static LOAD_COMMAND: Signal<CriticalSectionRawMutex, LoadCommand> = Signal::new();

fn vbus_is_present() -> bool {
    VBUS_PRESENT.lock(Cell::get)
}

fn publish_vbus_state(present: bool) {
    let changed = VBUS_PRESENT.lock(|state| {
        let changed = state.get() != present;
        state.set(present);
        changed
    });

    if changed {
        if present {
            VBUS_ATTACHED.signal(());
        } else {
            VBUS_DETACHED.signal(());
        }
    }
}

fn request_load(command: LoadCommand) {
    LOAD_COMMAND.signal(command);
}

fn controller_config() -> ControllerConfig {
    let epr_capable = cfg!(feature = "epr-capable-hardware");
    let pps_capable = cfg!(feature = "pps-capable-hardware");
    let limits = if epr_capable {
        SinkLimits {
            max_voltage: Some(Millivolts(48_000)),
            board_max_current: Some(Milliamps(5_000)),
            cable_max_current: None,
            max_power: Some(Milliwatts(140_000)),
        }
    } else if pps_capable {
        SinkLimits {
            max_voltage: Some(Millivolts(21_000)),
            board_max_current: Some(Milliamps(5_000)),
            cable_max_current: None,
            max_power: Some(Milliwatts(100_000)),
        }
    } else {
        SinkLimits {
            max_voltage: Some(Millivolts(5_000)),
            board_max_current: Some(Milliamps(3_000)),
            cable_max_current: None,
            max_power: Some(Milliwatts(15_000)),
        }
    };

    ControllerConfig {
        request_context: RequestContext {
            flags: RequestFlags { epr_capable, ..RequestFlags::default() },
            limits,
            source_present_pdp: None,
        },
        epr_operational_pdp: epr_capable.then_some(Milliwatts(140_000)),
    }
}

#[cfg(any(feature = "bench-pps-19v4", feature = "bench-epr-avs-19v4", feature = "bench-epr-fixed-48v"))]
#[embassy_executor::task]
async fn bench_request_task() {
    Timer::after_millis(2_000).await;

    #[cfg(feature = "bench-pps-19v4")]
    let request = UserRequest::Voltage { voltage: Millivolts(19_400), current: None, preference: Preference::Pps };
    #[cfg(feature = "bench-epr-avs-19v4")]
    let request = UserRequest::Voltage { voltage: Millivolts(19_400), current: None, preference: Preference::EprAvs };
    #[cfg(feature = "bench-epr-fixed-48v")]
    let request = UserRequest::Voltage { voltage: Millivolts(48_000), current: None, preference: Preference::Fixed };

    COMMANDS.send(Command::Request(request)).await;
}

macro_rules! logln {
    ($($arg:tt)*) => {{
        #[cfg(all(feature = "sdi-log", not(feature = "usb-console")))]
        hal::println!($($arg)*);
        #[cfg(all(feature = "usb-console", not(feature = "sdi-log")))]
        enqueue_console_log(core::format_args!($($arg)*));
        #[cfg(all(feature = "usb-console", feature = "sdi-log"))]
        enqueue_dual_log(core::format_args!($($arg)*));
        #[cfg(not(any(feature = "sdi-log", feature = "usb-console")))]
        let _ = core::format_args!($($arg)*);
    }};
}

/// Owns the physical load-control output for the lifetime of the firmware.
///
/// `PA6` is fed by a 3.3 V-safe VBUS power-good circuit. `PB12` is only the
/// firmware half of the load-enable equation; hardware must also gate the
/// switch directly with VBUS power-good so cable removal does not depend on
/// executor latency or working firmware.
#[embassy_executor::task]
async fn port_supervisor_task(mut vbus_present: ExtiInput<'static>, mut load_enable: Output<'static>) {
    load_enable.set_low();
    let mut needs_attach_debounce = true;

    loop {
        if vbus_present.is_low() {
            // This is the latency-critical path. Cut the MCU request before
            // doing any debounce, logging, or policy-engine work.
            load_enable.set_low();
            LOAD_COMMAND.reset();
            publish_vbus_state(false);
            vbus_present.wait_for_rising_edge().await;
            needs_attach_debounce = true;
            continue;
        }

        if needs_attach_debounce {
            Timer::after_millis(ATTACH_DEBOUNCE_MS).await;
            if vbus_present.is_low() {
                continue;
            }
            publish_vbus_state(true);
            needs_attach_debounce = false;
        }

        match select(vbus_present.wait_for_falling_edge(), LOAD_COMMAND.wait()).await {
            Either::First(()) => {
                load_enable.set_low();
                publish_vbus_state(false);
                needs_attach_debounce = true;
            }
            Either::Second(LoadCommand::Disable) => load_enable.set_low(),
            Either::Second(LoadCommand::Enable) => {
                if vbus_present.is_high() && vbus_is_present() {
                    load_enable.set_high();
                    // Close the edge race where VBUS fell just before the
                    // output instruction and its EXTI future has not run yet.
                    if vbus_present.is_low() {
                        load_enable.set_low();
                        publish_vbus_state(false);
                        needs_attach_debounce = true;
                    }
                } else {
                    load_enable.set_low();
                }
            }
        }
    }
}

#[cfg(not(feature = "usb-console"))]
bind_interrupts!(
    struct Irq {
        USBPD => InterruptHandler<peripherals::USBPD>;
    }
);

#[cfg(feature = "usb-console")]
bind_interrupts!(
    struct Irq {
        USBPD => InterruptHandler<peripherals::USBPD>;
        USBFS => UsbFsInterruptHandler;
    }
);

fn discard_pending_commands() {
    while COMMANDS.try_receive().is_ok() {}
}

fn log_device_identity() {
    let id = hal::signature::unique_id();

    // CH32X035 production silicon exposes the same first eight UID bytes as
    // the WCH USB bootloader. Although the reference manual describes a
    // 96-bit ESIG, the final word reads as erased (0xffff_ffff) on observed
    // X035 parts. Do not turn that unprogrammed word into the board label.
    let programmed_id = &id[..8];
    if programmed_id.iter().all(|byte| *byte == 0x00) || programmed_id.iter().all(|byte| *byte == 0xff) {
        logln!("Device id=unavailable");
        return;
    }

    let word0 = u32::from_be_bytes([id[0], id[1], id[2], id[3]]);
    let word1 = u32::from_be_bytes([id[4], id[5], id[6], id[7]]);
    logln!("Device id={:08x}{:08x}", word0, word1);
}

#[cfg(feature = "usb-console")]
fn log_console_help() {
    logln!("Commands:");
    logln!("device caps plans source-info status enter-epr epr-caps exit-epr help");
    logln!("request mV [mA|max] [auto|fixed|pps|spr-avs|epr-avs]");
    logln!("request mV [mA|max] epr-avs-nonstandard (explicit opt-in)");
    logln!("pdo N [max(=maxV APDO)|current mA|adjust mV [mA|max]]");
}

#[cfg(feature = "usb-console")]
async fn usb_console_rx(mut receiver: CdcReceiver<'static>) -> ! {
    let mut packet = [0u8; 64];
    let mut command = [0u8; CONSOLE_LINE_CAPACITY];
    let mut command_len = 0usize;
    let mut overflowed = false;

    loop {
        receiver.wait_connection().await;
        logln!("USB console ready; type help");
        log_device_identity();

        loop {
            let count = match receiver.read_packet(&mut packet).await {
                Ok(count) => count,
                Err(_) => break,
            };

            for &byte in &packet[..count] {
                match byte {
                    b'\r' => {}
                    b'\n' => {
                        if overflowed {
                            logln!("Command too long (max {})", CONSOLE_LINE_CAPACITY - 1);
                        } else if command_len != 0 {
                            match core::str::from_utf8(&command[..command_len])
                                .ok()
                                .and_then(|line| pd_sink::parse_command(line).ok())
                            {
                                Some(Command::Help) => log_console_help(),
                                Some(Command::Identity) => log_device_identity(),
                                Some(parsed) => match COMMANDS.try_send(parsed) {
                                    Ok(()) => logln!("Queued"),
                                    Err(_) => logln!("Busy; retry command"),
                                },
                                None => logln!("Invalid; type help"),
                            }
                        }
                        command_len = 0;
                        overflowed = false;
                    }
                    0x08 | 0x7f if !overflowed => {
                        command_len = command_len.saturating_sub(1);
                    }
                    byte if !overflowed && byte.is_ascii() && !byte.is_ascii_control() => {
                        if command_len < command.len() - 1 {
                            command[command_len] = byte;
                            command_len += 1;
                        } else {
                            overflowed = true;
                        }
                    }
                    _ => overflowed = true,
                }
            }
        }
    }
}

#[cfg(feature = "usb-console")]
async fn usb_console_tx(mut sender: CdcSender<'static>) -> ! {
    'connection: loop {
        sender.wait_connection().await;
        loop {
            let line = CONSOLE_LINES.receive().await;
            let bytes = line.as_bytes();
            for chunk in bytes.chunks(sender.max_packet_size()) {
                if sender.write_packet(chunk).await.is_err() {
                    continue 'connection;
                }
            }
            if bytes.len() % sender.max_packet_size() == 0 && sender.write_packet(&[]).await.is_err() {
                continue 'connection;
            }
        }
    }
}

fn validity_name(validity: PdoValidity) -> &'static str {
    match validity {
        PdoValidity::Valid => "valid",
        PdoValidity::Compatible => "compatible",
        PdoValidity::ZeroPadding => "padding",
        PdoValidity::Malformed(_) => "malformed",
        PdoValidity::Unsupported => "unsupported",
    }
}

#[cfg(not(all(feature = "usb-console", feature = "sdi-log")))]
fn log_controller_error(error: ControllerError) {
    let (reason, detail, extra) = match error {
        ControllerError::NoCapabilities(kind) => ("no-caps", kind as u32, 0),
        ControllerError::Busy(state) => ("epr-busy", state as u32, 0),
        ControllerError::EprUnavailable => ("epr-unavailable", 0, 0),
        ControllerError::EprNotConfigured => ("epr-not-configured", 0, 0),
        ControllerError::InvalidEprOperationalPdp(pdp) => ("epr-pdp", pdp.get(), 0),
        ControllerError::NotInEprMode => ("not-in-epr", 0, 0),
        ControllerError::Plan(plan) => match plan {
            PlanError::PositionUnavailable(position) => ("pdo-missing", u32::from(position), 0),
            PlanError::MalformedPdo { position, .. } => ("pdo-malformed", u32::from(position), 0),
            PlanError::UnsupportedPdo(position) => ("pdo-unsupported", u32::from(position), 0),
            PlanError::InvalidDemand { position, supply } => ("demand-kind", u32::from(position), supply as u32),
            PlanError::VoltageUnavailable(voltage) => ("voltage-unavailable", voltage.get(), 0),
            PlanError::VoltageOutsideOffer { position, requested } => {
                ("voltage-outside", u32::from(position), requested.get())
            }
            PlanError::VoltageAboveSinkLimit { requested, maximum } => {
                ("voltage-limit", requested.get(), maximum.get())
            }
            PlanError::CapabilitiesModeMismatch => ("caps-mode", 0, 0),
            PlanError::EprModeRequired(position) => ("epr-required", u32::from(position), 0),
            PlanError::ZeroOperatingValue => ("zero-operating", 0, 0),
        },
    };
    logln!("Rejected {} detail={} extra={}", reason, detail, extra);
}

#[cfg(all(feature = "usb-console", feature = "sdi-log"))]
fn log_controller_error(_error: ControllerError) {
    logln!("Command rejected");
}

fn log_product_capabilities(capabilities: &ProductSourceCapabilities) {
    logln!(
        "Source caps: kind={} count={} EPR={}",
        match capabilities.kind() {
            CapabilitiesKind::Spr => "SPR",
            CapabilitiesKind::Epr => "EPR",
        },
        capabilities.len(),
        capabilities.epr_mode_capable(),
    );

    for pdo in capabilities.iter() {
        let validity = validity_name(pdo.validity);
        match pdo.supply {
            SourceSupply::Fixed(fixed) => logln!(
                "PDO{} fixed {}mV {}mA EPR={} {} raw={:#010x}",
                pdo.position,
                fixed.voltage.get(),
                fixed.max_current.get(),
                fixed.epr_mode_capable,
                validity,
                pdo.raw
            ),
            SourceSupply::Pps(pps) => logln!(
                "PDO{} PPS {}-{}mV {}mA limited={} {} raw={:#010x}",
                pdo.position,
                pps.min_voltage.get(),
                pps.max_voltage.get(),
                pps.max_current.get(),
                pps.power_limited,
                validity,
                pdo.raw
            ),
            SourceSupply::SprAvs(avs) => logln!(
                "PDO{} SPR-AVS {}-{}mV {}mA@15V {}mA@20V peak={} {} raw={:#010x}",
                pdo.position,
                avs.min_voltage.get(),
                avs.max_voltage.get(),
                avs.max_current_15v.get(),
                avs.max_current_20v.get(),
                avs.peak_current,
                validity,
                pdo.raw
            ),
            SourceSupply::EprAvs(avs) => logln!(
                "PDO{} EPR-AVS {}-{}mV standard={}-{}mV PDP={}mW peak={} {} raw={:#010x}",
                pdo.position,
                avs.min_voltage.get(),
                avs.max_voltage.get(),
                avs.standard_min_voltage().get(),
                avs.max_voltage.get(),
                avs.pdp.get(),
                avs.peak_current,
                validity,
                pdo.raw
            ),
            SourceSupply::ZeroPadding => {
                logln!("PDO{} padding {} raw={:#010x}", pdo.position, validity, pdo.raw)
            }
            SourceSupply::Unsupported { pdo_type, apdo_type } => {
                logln!(
                    "PDO{} unsupported type={} apdo={} {} raw={:#010x}",
                    pdo.position,
                    pdo_type,
                    apdo_type.unwrap_or(0xff),
                    validity,
                    pdo.raw
                )
            }
        }
    }
}

fn raw_pdo(pdo: &PowerDataObject) -> u32 {
    match pdo {
        PowerDataObject::FixedSupply(fixed) => fixed.0,
        PowerDataObject::Battery(battery) => battery.0,
        PowerDataObject::VariableSupply(variable) => variable.0,
        PowerDataObject::Augmented(Augmented::Spr(pps)) => pps.0,
        PowerDataObject::Augmented(Augmented::SprAvs(avs)) => avs.0,
        PowerDataObject::Augmented(Augmented::Epr(avs)) => avs.0,
        PowerDataObject::Augmented(Augmented::Unknown(raw)) => *raw,
        PowerDataObject::Unknown(raw) => raw.0,
    }
}

fn product_capabilities(capabilities: &SourceCapabilities) -> ProductSourceCapabilities {
    let mut pdos = [0u32; pd_sink::capabilities::MAX_SOURCE_PDOS];
    let count = capabilities.pdos().len();
    assert!(count <= pdos.len(), "source advertised too many PDOs");

    for (destination, source) in pdos.iter_mut().zip(capabilities.pdos()) {
        *destination = raw_pdo(source);
    }

    let kind = if capabilities.is_epr_capabilities() { CapabilitiesKind::Epr } else { CapabilitiesKind::Spr };
    ProductSourceCapabilities::new(kind, &pdos[..count]).expect("source capability container has an invalid length")
}

fn protocol_request(plan: RequestPlan) -> PowerSource {
    if matches!(plan.message, RequestMessage::EprRequest) {
        return PowerSource::EprRequest(EprRequestDataObject {
            rdo: plan.rdo,
            pdo: parse_raw_pdo(plan.pdo_copy.expect("EPR Request must copy its selected PDO")),
        });
    }

    match plan.supply {
        SupplyKind::Fixed => PowerSource::FixedVariableSupply(request::FixedVariableSupply(plan.rdo)),
        SupplyKind::Pps => PowerSource::Pps(request::Pps(plan.rdo)),
        SupplyKind::SprAvs | SupplyKind::EprAvs => PowerSource::Avs(request::Avs(plan.rdo)),
        SupplyKind::ZeroPadding | SupplyKind::Unsupported => panic!("request planner selected an invalid PDO"),
    }
}

fn confidence_name(confidence: CurrentConfidence) -> &'static str {
    match confidence {
        CurrentConfidence::Advertised => "advertised",
        CurrentConfidence::DerivedFromPdoPdp => "derived-from-PDO-PDP",
        CurrentConfidence::DerivedFromSourceInfo => "derived-from-Source_Info",
        CurrentConfidence::PowerLimitedUpperBound => "power-limited-upper-bound",
    }
}

fn limit_name(limit: LimitReason) -> &'static str {
    match limit {
        LimitReason::Source => "source",
        LimitReason::Protocol => "protocol",
        LimitReason::Board => "board",
        LimitReason::Cable => "cable",
        LimitReason::SinkPower => "sink-power",
        LimitReason::User => "user",
    }
}

fn log_request_plan(prefix: &str, plan: RequestPlan) {
    match plan.voltage {
        PlannedVoltage::Fixed(voltage) => logln!(
            "{} PDO{} fixed={}mV EPR={}",
            prefix,
            plan.object_position,
            voltage.get(),
            u8::from(matches!(plan.message, RequestMessage::EprRequest))
        ),
        PlannedVoltage::Adjustable { requested, encoded, step_mv } => logln!(
            "{} PDO{} requested={}mV encoded={}mV step={}mV EPR={}",
            prefix,
            plan.object_position,
            requested.get(),
            encoded.get(),
            step_mv,
            u8::from(matches!(plan.message, RequestMessage::EprRequest))
        ),
    }

    match plan.operating {
        PlannedOperating::Current { requested, source_limit, operating, confidence, limited_by } => {
            if let Some(requested) = requested {
                logln!(
                    "{} req={}mA src={}mA usable={}mA confidence={} limit={} mismatch={}",
                    prefix,
                    requested.get(),
                    source_limit.get(),
                    operating.get(),
                    confidence_name(confidence),
                    limit_name(limited_by),
                    plan.capability_mismatch
                );
            } else {
                logln!(
                    "{} req=max src={}mA usable={}mA confidence={} limit={} mismatch={}",
                    prefix,
                    source_limit.get(),
                    operating.get(),
                    confidence_name(confidence),
                    limit_name(limited_by),
                    plan.capability_mismatch
                );
            }
        }
    }
}

struct EmbassySinkTimer;

impl SinkTimer for EmbassySinkTimer {
    async fn after_millis(milliseconds: u64) {
        Timer::after_millis(milliseconds).await;
    }
}

struct UsbpdSinkDriver<'d> {
    usbpd: UsbPdPhy<'d, peripherals::USBPD, hal::mode::Async>,
    last_sink_tx_ok: Option<bool>,
}

impl<'d> UsbpdSinkDriver<'d> {
    fn new(usbpd: UsbPdPhy<'d, peripherals::USBPD, hal::mode::Async>) -> Self {
        Self { usbpd, last_sink_tx_ok: None }
    }

    fn reset(&mut self) -> Result<(), UsbpdError> {
        self.last_sink_tx_ok = None;
        self.usbpd.reset()
    }
}

impl SinkDriver for UsbpdSinkDriver<'_> {
    async fn wait_for_vbus(&mut self) {
        while !vbus_is_present() {
            VBUS_ATTACHED.wait().await;
        }
        // A low pulse from a completed Hard Reset or an earlier physical
        // detach belongs to the old, already-invalidated session. Do not let
        // that stored signal immediately cancel the first receive of this
        // fresh startup.
        VBUS_DETACHED.reset();
        logln!("Attached; PD starts at 5 V");
    }

    fn sink_tx_ok(&mut self) -> bool {
        let sink_tx_ok = self.usbpd.sink_tx_ok();
        if self.last_sink_tx_ok != Some(sink_tx_ok) {
            if sink_tx_ok {
                logln!("SinkTxOK; resume");
            } else {
                logln!("SinkTxNG; deferred");
            }
            self.last_sink_tx_ok = Some(sink_tx_ok);
        }
        sink_tx_ok
    }

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, usbpd_traits::DriverRxError> {
        if !vbus_is_present() {
            request_load(LoadCommand::Disable);
            return Err(usbpd_traits::DriverRxError::Detached);
        }

        let received = match select(self.usbpd.receive(buffer), VBUS_DETACHED.wait()).await {
            Either::First(received) => received,
            Either::Second(()) => {
                request_load(LoadCommand::Disable);
                return Err(usbpd_traits::DriverRxError::Detached);
            }
        };

        if !vbus_is_present() {
            request_load(LoadCommand::Disable);
            return Err(usbpd_traits::DriverRxError::Detached);
        }

        match received {
            Ok((Sop::Sop, size)) => Ok(size),
            Ok(_) => Err(usbpd_traits::DriverRxError::Discarded),
            Err(UsbpdError::HardReset) => {
                request_load(LoadCommand::Disable);
                Err(usbpd_traits::DriverRxError::HardReset)
            }
            Err(_) => Err(usbpd_traits::DriverRxError::Discarded),
        }
    }

    async fn transmit(&mut self, data: &[u8]) -> Result<(), usbpd_traits::DriverTxError> {
        if !vbus_is_present() {
            request_load(LoadCommand::Disable);
            return Err(usbpd_traits::DriverTxError::Detached);
        }

        let transmitted = match select(self.usbpd.transmit(data), VBUS_DETACHED.wait()).await {
            Either::First(transmitted) => transmitted,
            Either::Second(()) => {
                request_load(LoadCommand::Disable);
                return Err(usbpd_traits::DriverTxError::Detached);
            }
        };

        if !vbus_is_present() {
            request_load(LoadCommand::Disable);
            return Err(usbpd_traits::DriverTxError::Detached);
        }

        transmitted.map_err(|error| match error {
            UsbpdError::HardReset => {
                request_load(LoadCommand::Disable);
                usbpd_traits::DriverTxError::HardReset
            }
            _ => usbpd_traits::DriverTxError::Discarded,
        })
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), usbpd_traits::DriverTxError> {
        request_load(LoadCommand::Disable);
        if !vbus_is_present() {
            return Err(usbpd_traits::DriverTxError::Detached);
        }

        match select(self.usbpd.transmit_hardreset(), VBUS_DETACHED.wait()).await {
            Either::First(result) => result.map_err(|error| match error {
                UsbpdError::HardReset => usbpd_traits::DriverTxError::HardReset,
                _ => usbpd_traits::DriverTxError::Discarded,
            }),
            Either::Second(()) => Err(usbpd_traits::DriverTxError::Detached),
        }
    }
}

struct Device {
    controller: SinkController,
    contract: ContractTracker,
    source_info_requested: bool,
    epr_discovery_attempts: u8,
    epr_exhaustion_reported: bool,
}

impl Device {
    fn new() -> Self {
        Self {
            controller: SinkController::new(controller_config()),
            contract: ContractTracker::new(),
            source_info_requested: false,
            epr_discovery_attempts: 0,
            epr_exhaustion_reported: false,
        }
    }

    fn begin_request(&mut self, plan: RequestPlan) {
        // Do not expose a changing supply to the load. An identical PPS
        // maintenance request keeps the confirmed contract and load stable;
        // a request that changes any encoded operating parameter cuts it.
        if self.contract.request_changes_power(plan) {
            request_load(LoadCommand::Disable);
        }
        self.contract.on_request(plan).expect("request must follow advertised capabilities");
    }

    fn log_confirmed_contract(&self) {
        if let Some(plan) = self.contract.active_plan() {
            log_request_plan("Contract ready", plan);
        } else {
            logln!("No confirmed contract");
        }
    }

    #[cfg(not(all(feature = "usb-console", feature = "sdi-log")))]
    fn log_capability_plans(&self, source_capabilities: &SourceCapabilities) {
        let capabilities = product_capabilities(source_capabilities);
        logln!("Capability plans: count={} (live contract unchanged)", capabilities.len());
        for pdo in capabilities.iter() {
            if !pdo.is_requestable() {
                logln!("Plan PDO{} unavailable {}", pdo.position, validity_name(pdo.validity));
                continue;
            }

            match self
                .controller
                .preview(pd_sink::UserRequest::Pdo { position: pdo.position, demand: pd_sink::Demand::Maximum })
            {
                Ok(plan) => log_request_plan("Plan", plan),
                Err(error) => log_controller_error(error),
            }
        }
        self.log_confirmed_contract();
    }

    #[cfg(all(feature = "usb-console", feature = "sdi-log"))]
    fn log_capability_plans(&self, _source_capabilities: &SourceCapabilities) {
        logln!("Plans unavailable in dual-log; use usb-epr");
    }

    fn event_for_action(&mut self, action: ControllerAction) -> Event {
        match action {
            ControllerAction::Request(plan) => {
                self.begin_request(plan);
                Event::RequestPower(protocol_request(plan))
            }
            ControllerAction::EnterEprMode { operational_pdp } => {
                Event::enter_epr_mode_watts((operational_pdp.get() / 1_000) as u8)
            }
            ControllerAction::RequestEprCapabilities => Event::RequestEprSourceCapabilities,
            ControllerAction::ExitEprMode => Event::ExitEprMode,
            ControllerAction::None => Event::None,
        }
    }
}

impl DevicePolicyManager for Device {
    fn sink_capabilities(&self) -> SinkCapabilities {
        let current_10ma =
            if cfg!(any(feature = "pps-capable-hardware", feature = "epr-capable-hardware")) { 500 } else { 300 };
        SinkCapabilities::new_vsafe5v_only(current_10ma)
    }

    fn sink_capabilities_extended(&self) -> SinkCapabilitiesExtended {
        let epr_capable = cfg!(feature = "epr-capable-hardware");
        let programmable = epr_capable || cfg!(feature = "pps-capable-hardware");
        let mut sink_modes = SINK_MODE_VBUS_POWERED;
        if programmable {
            sink_modes |= SINK_MODE_PPS_SUPPORTED;
        }
        if epr_capable {
            sink_modes |= SINK_MODE_AVS_SUPPORTED;
        }

        SinkCapabilitiesExtended::new_v1_power_descriptor(
            // Temporary WCH development identifiers, matching the USB
            // descriptor. Replace before distributing hardware.
            0x1a86,
            0xfe0c,
            sink_modes,
            5,
            15,
            if programmable { 100 } else { 15 },
            if epr_capable { 5 } else { 0 },
            // Must match Event::enter_epr_mode_watts() below.
            if epr_capable { 140 } else { 0 },
            if epr_capable { 140 } else { 0 },
        )
    }

    async fn inform(&mut self, source_capabilities: &SourceCapabilities) {
        if matches!(self.contract.state(), ContractState::Detached | ContractState::Lost) {
            self.contract.on_attach();
        }
        self.contract.on_capabilities().expect("capabilities require an attached port");

        let product = product_capabilities(source_capabilities);
        log_product_capabilities(&product);
        self.controller.observe_capabilities(product);
        self.source_info_requested = false;
    }

    async fn request(&mut self, source_capabilities: &SourceCapabilities) -> PowerSource {
        let capabilities = product_capabilities(source_capabilities);
        let plan = self
            .controller
            .request_for_capabilities(capabilities)
            .expect("every compliant source must advertise a valid 5 V fixed PDO");

        self.begin_request(plan);
        log_request_plan("Requesting", plan);
        logln!("RDO={:#010x}", plan.rdo);
        protocol_request(plan)
    }

    async fn transition_power(&mut self, _accepted: &PowerSource) {
        // The policy engine invokes this only after receiving PS_RDY. Until
        // this point the requested current must not be presented to the load.
        self.contract.on_accept().expect("PS_RDY must correspond to a pending request");
        self.contract.on_ps_ready().expect("accepted request must become the active contract");
        self.controller.on_ps_ready();
        request_load(LoadCommand::Enable);
        self.log_confirmed_contract();
    }

    async fn request_not_accepted(&mut self, reason: RequestRejection) {
        self.contract.on_reject_or_wait();
        match reason {
            RequestRejection::Reject => self.controller.request_rejected(),
            RequestRejection::Wait => self.controller.request_deferred(),
        }
        if self.contract.load_may_enable() {
            request_load(LoadCommand::Enable);
        }
        match reason {
            RequestRejection::Reject => logln!("Request rejected; old contract active"),
            RequestRejection::Wait => logln!("Request deferred; retry armed"),
        }
    }

    async fn inform_source_info(&mut self, source_info: &SourceInfo) {
        let watts = source_info.port_present_pdp_watts();
        let pdp = (watts != 0).then_some(Milliwatts(u32::from(watts) * 1_000));
        self.controller.set_source_present_pdp(pdp);
        logln!(
            "Source_Info: present={} W, maximum={} W, reported={} W",
            watts,
            source_info.object1.port_maximum_pdp_watts(),
            source_info.object1.port_reported_pdp_watts()
        );
    }

    async fn hard_reset(&mut self, origin: HardResetOrigin) {
        request_load(LoadCommand::Disable);
        discard_pending_commands();
        self.contract.on_protocol_loss();
        self.controller.reset_port();
        self.source_info_requested = false;
        match origin {
            HardResetOrigin::Source => {
                logln!("Hard reset received; load off; wait={}ms", HARD_RESET_RECOVERY_MS)
            }
            HardResetOrigin::Sink => {
                logln!("Hard reset sent; load off; wait={}ms", HARD_RESET_RECOVERY_MS)
            }
        }

        // Let even the slow EPR-to-default source timing complete before the
        // Sink listens for fresh SPR capabilities. This recovery path does
        // not require PA6 to pulse: the isolated fixture may hold it high.
        Timer::after_millis(HARD_RESET_RECOVERY_MS).await;
        logln!("Reset wait complete; listen SPR");
    }

    async fn detached(&mut self) {
        request_load(LoadCommand::Disable);
        discard_pending_commands();
        self.contract.on_detach();
        self.controller.reset_port();
        self.source_info_requested = false;
        self.epr_discovery_attempts = 0;
        self.epr_exhaustion_reported = false;
        logln!("Detached; contract lost; load off");
    }

    async fn protocol_lost(&mut self) {
        request_load(LoadCommand::Disable);
        discard_pending_commands();
        self.contract.on_protocol_loss();
        self.controller.reset_port();
        self.source_info_requested = false;
        // Preserve the bounded automatic EPR attempt budget. Restarting the
        // software session without a physical detach must not create another
        // unlimited series of EPR entries.
        logln!("Protocol lost; load off; EPR={}/{}", self.epr_discovery_attempts, MAX_AUTO_EPR_ATTEMPTS);
    }

    async fn epr_mode_entry_failed(&mut self, reason: DataEnterFailed) {
        self.controller.epr_entry_failed();
        self.source_info_requested = false;
        // An explicit EnterFailed response describes a persistent source or
        // cable decision. Remain useful in SPR and leave any later EPR retry
        // to an explicit user command.
        self.epr_discovery_attempts = MAX_AUTO_EPR_ATTEMPTS;
        logln!("EPR entry failed reason={}; auto off", u8::from(reason));
    }

    async fn get_event(&mut self, source_capabilities: &SourceCapabilities) -> Event {
        loop {
            if let Some(action) = self.controller.take_ready_action() {
                return self.event_for_action(action);
            }

            if self.contract.load_may_enable() && !self.source_info_requested {
                self.source_info_requested = true;
                return Event::RequestSourceInfo;
            }

            #[cfg(feature = "epr-capable-hardware")]
            if self.contract.load_may_enable()
                && self.source_info_requested
                && matches!(self.controller.epr_state(), EprState::Spr)
                && source_capabilities.epr_mode_capable()
                && self.epr_discovery_attempts < MAX_AUTO_EPR_ATTEMPTS
            {
                match self.controller.begin_epr_discovery() {
                    Ok(action) => {
                        self.epr_discovery_attempts += 1;
                        self.epr_exhaustion_reported = false;
                        logln!(
                            "EPR discovery: enter attempt={}/{}; hold 5 V",
                            self.epr_discovery_attempts,
                            MAX_AUTO_EPR_ATTEMPTS
                        );
                        return self.event_for_action(action);
                    }
                    // A Source may first advertise a temporary 5 V-only
                    // capability set and replace it shortly afterward. That
                    // is not an EPR attempt and must not consume the retry
                    // budget. The guard above normally filters this case; the
                    // match also closes a capability-update race.
                    Err(ControllerError::EprUnavailable | ControllerError::NoCapabilities(_)) => {}
                    Err(_) => {
                        self.epr_discovery_attempts = MAX_AUTO_EPR_ATTEMPTS;
                        logln!("EPR discovery unavailable; auto off");
                    }
                }
            }

            #[cfg(feature = "epr-capable-hardware")]
            if self.contract.load_may_enable()
                && self.source_info_requested
                && matches!(self.controller.epr_state(), EprState::Spr)
                && self.epr_discovery_attempts >= MAX_AUTO_EPR_ATTEMPTS
                && !self.epr_exhaustion_reported
            {
                self.epr_exhaustion_reported = true;
                logln!("EPR auto off; staying SPR; manual retry available");
            }

            let command = COMMANDS.receive().await;
            let action = match command {
                Command::Request(request) => self.controller.submit(request),
                Command::Identity => {
                    log_device_identity();
                    continue;
                }
                Command::Capabilities => {
                    let product = product_capabilities(source_capabilities);
                    log_product_capabilities(&product);
                    continue;
                }
                Command::Plans => {
                    self.log_capability_plans(source_capabilities);
                    continue;
                }
                Command::RequestSourceInfo => {
                    self.source_info_requested = true;
                    return Event::RequestSourceInfo;
                }
                Command::EnterEpr => {
                    let action = self.controller.begin_epr_discovery();
                    if action.is_ok() {
                        logln!("EPR manual enter; hold 5 V");
                    }
                    action
                }
                Command::RequestEprCapabilities => self.controller.request_epr_capabilities(),
                Command::ExitEpr => self.controller.exit_epr(),
                Command::Status => {
                    self.log_confirmed_contract();
                    continue;
                }
                Command::Help => {
                    continue;
                }
            };

            match action {
                Ok(action) => return self.event_for_action(action),
                Err(error) => log_controller_error(error),
            }
        }
    }
}

async fn run_pd(phy: UsbPdPhy<'static, peripherals::USBPD, hal::mode::Async>) -> ! {
    let driver = UsbpdSinkDriver::new(phy);
    let mut sink: Sink<_, EmbassySinkTimer, _> = Sink::new(driver, Device::new());

    loop {
        while let Err(error) = sink.driver_mut().reset() {
            if !matches!(error, UsbpdError::CCNotConnected) {
                logln!("PD reset failed");
            }
            Timer::after_millis(20).await;
        }

        sink.restart();
        logln!("CC detected; waiting for VBUS");

        let result = sink.run().await;
        let restart_delay_ms = match result {
            Ok(()) => {
                logln!("PD stopped");
                20
            }
            Err(usbpd::sink::policy_engine::Error::Detached) => {
                logln!("PD stopped: detach");
                20
            }
            Err(usbpd::sink::policy_engine::Error::PhyUnstable) => {
                logln!("PD stopped: PHY; retry={}ms", PROTOCOL_RESTART_COOLDOWN_MS);
                PROTOCOL_RESTART_COOLDOWN_MS
            }
            Err(usbpd::sink::policy_engine::Error::PortPartnerUnresponsive) => {
                logln!("PD stopped: timeout; retry={}ms", PROTOCOL_RESTART_COOLDOWN_MS);
                PROTOCOL_RESTART_COOLDOWN_MS
            }
            Err(usbpd::sink::policy_engine::Error::Protocol(_)) => {
                logln!("PD stopped: protocol; retry={}ms", PROTOCOL_RESTART_COOLDOWN_MS);
                PROTOCOL_RESTART_COOLDOWN_MS
            }
            Err(usbpd::sink::policy_engine::Error::InvalidEprOperationalPdp)
            | Err(usbpd::sink::policy_engine::Error::InvalidRequestForMode) => {
                logln!("PD stopped: policy; retry={}ms", PROTOCOL_RESTART_COOLDOWN_MS);
                PROTOCOL_RESTART_COOLDOWN_MS
            }
        };
        discard_pending_commands();
        Timer::after_millis(restart_delay_ms).await;
    }
}

#[embassy_executor::main(entry = "qingke_rt::entry")]
async fn main(_spawner: Spawner) {
    #[cfg(feature = "sdi-log")]
    hal::debug::SDIPrint::enable();

    #[cfg(any(feature = "bench-pps-19v4", feature = "bench-epr-avs-19v4", feature = "bench-epr-fixed-48v"))]
    _spawner.spawn(bench_request_task().expect("bench command task allocation failed"));

    let config = hal::Config { rcc: hal::rcc::Config::SYSCLK_FREQ_48MHZ_HSI, ..Default::default() };
    let peripherals = hal::init(config);

    let vbus_present = ExtiInput::new(peripherals.PA6, peripherals.EXTI6, Pull::Down);
    let load_enable = Output::new(peripherals.PB12, Level::Low, Speed::Low);
    _spawner.spawn(port_supervisor_task(vbus_present, load_enable).expect("port supervisor task allocation failed"));

    let phy = UsbPdPhy::new_async(peripherals.USBPD, peripherals.PC14, peripherals.PC15, Irq);

    #[cfg(not(feature = "usb-console"))]
    run_pd(phy).await;

    #[cfg(feature = "usb-console")]
    {
        let cdc = CdcAcm::new(peripherals.USBFS, peripherals.PC16, peripherals.PC17, Irq);
        let (sender, receiver) = cdc.split();
        join3(usb_console_rx(receiver), usb_console_tx(sender), run_pd(phy)).await;
    }
}
