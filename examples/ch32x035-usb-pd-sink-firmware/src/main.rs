#![no_std]
#![no_main]
#![forbid(unsafe_code)]

use core::cell::Cell;
#[cfg(feature = "dev-text-console")]
use core::fmt::{self, Write};

#[cfg(feature = "usb-control")]
mod control_transport;
#[cfg(feature = "rev0-board")]
mod rev0_validation;

use ch32_hal as hal;
use ch32_hal::exti::ExtiInput;
#[cfg(not(feature = "rev0-board"))]
use ch32_hal::gpio::Pull;
use ch32_hal::gpio::{Level, Output, Speed};
#[cfg(any(feature = "usb-control", feature = "dev-text-console"))]
use ch32_hal::usb_x0fs::cdc::{CdcAcm, InterruptHandler as UsbFsInterruptHandler};
#[cfg(feature = "dev-text-console")]
use ch32_hal::usb_x0fs::cdc::{Receiver as CdcReceiver, Sender as CdcSender};
use ch32_hal::usbpd::{InterruptHandler, UsbPdPhy};
use ch32_hal::{bind_interrupts, peripherals};
use embassy_executor::Spawner;
#[cfg(any(feature = "usb-control", feature = "dev-text-console"))]
use embassy_futures::join::join3;
use embassy_futures::select::{select, select3, Either, Either3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Instant, Timer};
use panic_halt as _;
use pd_sink::PlanError;
use pd_sink::{
    CapabilitiesKind, Ch32x035Port, Ch32x035SessionTimer, Ch32x035SinkSession, Command, ContractTransition,
    ControllerConfig, ControllerError, CurrentConfidence, HardResetDirection, LimitReason, LoadControlState, Milliamps,
    Millivolts, Milliwatts, PhyEvent, PlannedOperating, PlannedVoltage, PpsStatus, RequestContext, RequestFlags,
    RequestMessage, RequestPlan, RequestResult, SinkConfig, SinkLimits, SinkPowerDescriptor, SinkRuntime,
    SinkSessionEvent, SinkSessionRecovery, SinkSessionTerminalError, SourceAlert,
    SourceCapabilities as ProductSourceCapabilities, SourceStatus, StatusQuery, StatusQueryFailure,
    TransitionLoadPolicy,
};
#[cfg(not(feature = "dev-text-console"))]
use pd_sink::{CapabilityPlan, PdoValidity, SourceSupply};
#[cfg(feature = "usb-control")]
use pd_sink::{
    ControlEprEvent, ControlEvent, ControlIntegrationError, ControlLifecycleEvent, ControlPlanStage, DeviceInfo,
    HardResetCause,
};

#[cfg(all(feature = "usb-control", feature = "dev-text-console"))]
compile_error!("usb-control and dev-text-console are separate wire protocols; select only one");
#[cfg(all(feature = "sdi-log", feature = "dev-text-console"))]
compile_error!("sdi-log and dev-text-console are separate diagnostic outputs; select only one");
#[cfg(all(feature = "rev0-board", not(feature = "ch32x035g8u6")))]
compile_error!("rev0-board is only valid for the CH32X035G8U6");
#[cfg(all(feature = "rev0-board", not(feature = "output-default-off")))]
compile_error!("rev0-board requires output-default-off");
#[cfg(all(feature = "rev0-validation", not(feature = "dev-text-console")))]
compile_error!("rev0-validation requires dev-text-console diagnostics");
#[cfg(all(feature = "rev0-validation", any(feature = "pps-capable-hardware", feature = "epr-capable-hardware")))]
compile_error!("rev0-validation is a fixed-5-V validation profile; do not enable PPS or EPR hardware features");

static COMMANDS: Channel<CriticalSectionRawMutex, Command, 4> = Channel::new();

#[cfg(feature = "dev-text-console")]
const CONSOLE_LINE_CAPACITY: usize = 128;
#[cfg(feature = "dev-text-console")]
const CONSOLE_QUEUE_DEPTH: usize = 32;

#[cfg(feature = "dev-text-console")]
#[derive(Clone, Copy)]
struct ConsoleLine {
    bytes: [u8; CONSOLE_LINE_CAPACITY],
    len: u8,
}

#[cfg(feature = "dev-text-console")]
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

#[cfg(feature = "dev-text-console")]
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

#[cfg(feature = "dev-text-console")]
static CONSOLE_LINES: Channel<CriticalSectionRawMutex, ConsoleLine, CONSOLE_QUEUE_DEPTH> = Channel::new();

#[cfg(feature = "dev-text-console")]
fn enqueue_console_log(arguments: fmt::Arguments<'_>) {
    let mut line = ConsoleLine::new();
    let _ = line.write_fmt(arguments);
    line.finish();
    let _ = CONSOLE_LINES.try_send(line);
}

const ATTACH_DEBOUNCE_MS: u64 = 100;
const HARD_RESET_RECOVERY_MS: u64 = 2_000;
const MAX_AUTO_EPR_ATTEMPTS: u8 = 2;
#[cfg(not(feature = "uninterrupted-load-transitions"))]
const TRANSITION_LOAD_POLICY: TransitionLoadPolicy = TransitionLoadPolicy::InhibitUntilReady;
#[cfg(feature = "uninterrupted-load-transitions")]
const TRANSITION_LOAD_POLICY: TransitionLoadPolicy = TransitionLoadPolicy::Uninterrupted;
const EPR_OPERATIONAL_PDP_WATTS: u8 = 240;
const USER_OUTPUT_DEFAULT_ENABLED: bool = !cfg!(feature = "output-default-off");

static VBUS_PRESENT: Mutex<CriticalSectionRawMutex, Cell<bool>> = Mutex::new(Cell::new(false));
static VBUS_ATTACHED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static VBUS_DETACHED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static PD_LOAD_CONTROL: Mutex<CriticalSectionRawMutex, Cell<LoadControlState>> =
    Mutex::new(Cell::new(LoadControlState::unmanaged()));
static PD_LOAD_CHANGED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static USER_OUTPUT_REQUEST: Signal<CriticalSectionRawMutex, bool> = Signal::new();

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

fn set_pd_load_permitted(permitted: bool) {
    PD_LOAD_CONTROL.lock(|state| {
        let mut next = state.get();
        next.set_pd_permitted(permitted);
        state.set(next);
    });
    PD_LOAD_CHANGED.signal(());
}

fn apply_transition_load_policy(policy: TransitionLoadPolicy, transition: ContractTransition) {
    PD_LOAD_CONTROL.lock(|state| {
        let mut next = state.get();
        next.begin_transition(policy, transition);
        state.set(next);
    });
    PD_LOAD_CHANGED.signal(());
}

fn set_pd_load_unmanaged() {
    PD_LOAD_CONTROL.lock(|state| {
        let mut next = state.get();
        next.set_unmanaged();
        state.set(next);
    });
    PD_LOAD_CHANGED.signal(());
}

fn reset_pd_load_control() {
    PD_LOAD_CONTROL.lock(|state| state.set(LoadControlState::unmanaged()));
    PD_LOAD_CHANGED.reset();
}

fn take_pd_load_control() -> (LoadControlState, bool) {
    PD_LOAD_CONTROL.lock(|state| {
        let mut current = state.get();
        let clear_user_latch = current.take_user_latch_clear();
        state.set(current);
        (current, clear_user_latch)
    })
}

fn set_user_output_enabled(enabled: bool) {
    USER_OUTPUT_REQUEST.signal(enabled);
}

fn board_limits() -> SinkLimits {
    let epr_capable = cfg!(feature = "epr-capable-hardware");
    let epr_50v_compatible = cfg!(feature = "epr-50v-compatible-hardware");
    let pps_capable = cfg!(feature = "pps-capable-hardware");
    if epr_50v_compatible {
        SinkLimits {
            max_voltage: Some(Millivolts(50_000)),
            board_max_current: Some(Milliamps(5_000)),
            cable_max_current: Some(Milliamps(5_000)),
            max_power: Some(Milliwatts(240_000)),
        }
    } else if epr_capable {
        SinkLimits {
            max_voltage: Some(Millivolts(48_000)),
            board_max_current: Some(Milliamps(5_000)),
            cable_max_current: None,
            max_power: Some(Milliwatts(240_000)),
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
    }
}

fn sink_config() -> SinkConfig {
    let epr_capable = cfg!(feature = "epr-capable-hardware");
    let pps_capable = cfg!(feature = "pps-capable-hardware");
    let limits = board_limits();

    SinkConfig {
        controller: ControllerConfig {
            request_context: RequestContext {
                flags: RequestFlags { epr_capable, ..RequestFlags::default() },
                limits,
                source_present_pdp: None,
            },
            epr_operational_pdp: epr_capable.then_some(Milliwatts(u32::from(EPR_OPERATIONAL_PDP_WATTS) * 1_000)),
        },
        descriptor: SinkPowerDescriptor {
            // Temporary WCH development identifiers, matching the USB
            // descriptor. Replace before distributing hardware.
            vendor_id: 0x1a86,
            product_id: 0xfe0c,
            maximum_current: if epr_capable || pps_capable { Milliamps(5_000) } else { Milliamps(3_000) },
            pps_supported: epr_capable || pps_capable,
            avs_supported: epr_capable,
            spr_minimum_pdp_watts: 5,
            spr_operational_pdp_watts: 15,
            spr_maximum_pdp_watts: if epr_capable || pps_capable { 100 } else { 15 },
            epr_minimum_pdp_watts: if epr_capable { 5 } else { 0 },
            epr_operational_pdp_watts: if epr_capable { EPR_OPERATIONAL_PDP_WATTS } else { 0 },
            epr_maximum_pdp_watts: if epr_capable { 240 } else { 0 },
        },
        // The normal profiles preserve the user's latch, inhibit PB10 during
        // an electrical transition, and restore it after PS_RDY. The explicit
        // uninterrupted profile keeps PB10 asserted across the transition.
        transition_load_policy: TRANSITION_LOAD_POLICY,
        max_auto_epr_attempts: if epr_capable { MAX_AUTO_EPR_ATTEMPTS } else { 0 },
        hard_reset_recovery_ms: HARD_RESET_RECOVERY_MS,
    }
}

#[cfg(any(feature = "usb-control", feature = "dev-text-console", feature = "sdi-log"))]
fn programmed_device_uid() -> Option<[u8; 8]> {
    let id = hal::signature::unique_id();
    let mut programmed = [0; 8];
    programmed.copy_from_slice(&id[..8]);
    if programmed.iter().all(|byte| *byte == 0x00) || programmed.iter().all(|byte| *byte == 0xff) {
        None
    } else {
        Some(programmed)
    }
}

#[cfg(feature = "usb-control")]
fn device_info() -> DeviceInfo {
    let limits = board_limits();
    let epr_capable = cfg!(feature = "epr-capable-hardware");
    let pps_capable = epr_capable || cfg!(feature = "pps-capable-hardware");
    DeviceInfo {
        uid: programmed_device_uid().unwrap_or([0; 8]),
        flags: (u8::from(pps_capable) * DeviceInfo::PPS_SUPPORTED)
            | (u8::from(epr_capable) * DeviceInfo::EPR_SUPPORTED),
        max_voltage: limits.max_voltage.unwrap_or(Millivolts(5_000)),
        max_current: limits.board_max_current.unwrap_or(Milliamps(3_000)),
        max_power: limits.max_power.unwrap_or(Milliwatts(15_000)),
    }
}

macro_rules! logln {
    ($($arg:tt)*) => {{
        #[cfg(feature = "sdi-log")]
        hal::println!($($arg)*);
        #[cfg(feature = "dev-text-console")]
        enqueue_console_log(core::format_args!($($arg)*));
        #[cfg(not(any(feature = "sdi-log", feature = "dev-text-console")))]
        let _ = core::format_args!($($arg)*);
    }};
}

macro_rules! control_event {
    ($event:expr) => {{
        #[cfg(feature = "usb-control")]
        control_transport::try_emit($event);
    }};
}

/// Owns the board-provided active-high minimum-VBUS input and load-enable
/// output for the lifetime of the firmware.
///
/// Cargo builds without the rev0 board binding provide those signals on PA6
/// and PA7. Scripted USB profiles supply OPA1 via the PB1/PB5 bonded pad and
/// PB10 from the rev0 board-local module. The reusable PD library does not
/// know these pins.
#[embassy_executor::task]
async fn port_supervisor_task(
    mut vbus_present: ExtiInput<'static>,
    mut load_enable: Output<'static>,
    detector_ready: bool,
) {
    load_enable.set_low();
    if !detector_ready {
        publish_vbus_state(false);
        reset_pd_load_control();
        #[cfg(feature = "rev0-board")]
        logln!("Rev0 detector fault: register readback invalid; PB10=0");
        loop {
            Timer::after_millis(60_000).await;
        }
    }

    let mut needs_attach_debounce = true;
    // Custom builds may preserve the historical automatic-on behavior.
    // Scripted rev0 profiles require an explicit Output On command so a test
    // image cannot energize a product load merely by completing negotiation.
    let mut user_output_enabled = USER_OUTPUT_DEFAULT_ENABLED;
    #[cfg(feature = "rev0-board")]
    let mut reported_load_enabled = false;

    #[cfg(feature = "rev0-board")]
    logln!(
        "Rev0 safety: PB10=0 OPA1/PB1 raw={} qualified=0 pd=0 user={}",
        u8::from(vbus_present.is_high()),
        u8::from(user_output_enabled),
    );

    loop {
        if vbus_present.is_low() {
            // This is the latency-critical path. Cut the MCU request before
            // doing any debounce, logging, or policy-engine work.
            load_enable.set_low();
            reset_pd_load_control();
            publish_vbus_state(false);
            #[cfg(feature = "rev0-board")]
            {
                user_output_enabled = false;
                USER_OUTPUT_REQUEST.reset();
            }
            vbus_present.wait_for_high().await;
            needs_attach_debounce = true;
            continue;
        }

        if needs_attach_debounce {
            #[cfg(feature = "rev0-board")]
            logln!("Rev0 OPA1/PB1 raw=1; qualify={}ms", ATTACH_DEBOUNCE_MS);
            // Give deassertion priority if the low event and timer become
            // ready in the same executor poll.
            match select(vbus_present.wait_for_low(), Timer::after_millis(ATTACH_DEBOUNCE_MS)).await {
                Either::Second(()) if vbus_present.is_high() => {}
                Either::First(()) | Either::Second(()) => {
                    load_enable.set_low();
                    reset_pd_load_control();
                    publish_vbus_state(false);
                    #[cfg(feature = "rev0-board")]
                    {
                        user_output_enabled = false;
                        USER_OUTPUT_REQUEST.reset();
                    }
                    #[cfg(feature = "rev0-board")]
                    logln!("Rev0 OPA1/PB1 raw=0 qualified=0; assertion cancelled");
                    continue;
                }
            }
            #[cfg(feature = "rev0-board")]
            USER_OUTPUT_REQUEST.reset();
            publish_vbus_state(true);
            needs_attach_debounce = false;
            #[cfg(feature = "rev0-board")]
            logln!("Rev0 OPA1/PB1 raw=1 qualified=1; attach published");
        }

        let (pd_policy_active, pd_load_permitted, uninterrupted_transition) =
            match select3(vbus_present.wait_for_low(), PD_LOAD_CHANGED.wait(), USER_OUTPUT_REQUEST.wait()).await {
                Either3::First(()) => {
                    load_enable.set_low();
                    reset_pd_load_control();
                    publish_vbus_state(false);
                    needs_attach_debounce = true;
                    #[cfg(feature = "rev0-board")]
                    {
                        user_output_enabled = false;
                        USER_OUTPUT_REQUEST.reset();
                    }
                    #[cfg(feature = "rev0-board")]
                    logln!("Rev0 OPA1/PB1 raw=0 qualified=0 pd=0 PB10=0; detach immediate");
                    (false, false, false)
                }
                Either3::Second(()) => {
                    let (control, clear_user_latch) = take_pd_load_control();
                    if clear_user_latch {
                        user_output_enabled = false;
                        USER_OUTPUT_REQUEST.reset();
                    }
                    #[cfg(feature = "rev0-board")]
                    logln!(
                        "Rev0 load policy: managed={} pd={} bypass={} user={}",
                        u8::from(control.policy_active()),
                        u8::from(control.pd_permitted()),
                        u8::from(control.uninterrupted_transition()),
                        u8::from(user_output_enabled),
                    );
                    (control.policy_active(), control.pd_permitted(), control.uninterrupted_transition())
                }
                Either3::Third(enabled) => {
                    // A safety action and Output On can become ready in the same
                    // executor poll. Consume the sticky cutoff first so the user
                    // command cannot win that race.
                    let (control, clear_user_latch) = take_pd_load_control();
                    let enabled = enabled && !clear_user_latch;
                    #[cfg(not(feature = "rev0-board"))]
                    {
                        user_output_enabled = enabled;
                    }
                    #[cfg(feature = "rev0-board")]
                    {
                        let pd_allows_load = control.pd_allows_load();
                        let can_arm = pd_allows_load && vbus_present.is_high() && vbus_is_present();
                        user_output_enabled = enabled && can_arm;
                        logln!(
                            "Rev0 user permission: requested={} accepted={} managed={} pd={}",
                            u8::from(enabled),
                            u8::from(user_output_enabled),
                            u8::from(control.policy_active()),
                            u8::from(control.pd_permitted()),
                        );
                    }
                    (control.policy_active(), control.pd_permitted(), control.uninterrupted_transition())
                }
            };

        let pd_allows_load = !pd_policy_active || pd_load_permitted || uninterrupted_transition;
        if pd_allows_load && user_output_enabled && vbus_present.is_high() && vbus_is_present() {
            load_enable.set_high();
            // Close the edge race where VBUS fell just before the output
            // instruction and its EXTI future has not run yet.
            if vbus_present.is_low() {
                load_enable.set_low();
                reset_pd_load_control();
                publish_vbus_state(false);
                needs_attach_debounce = true;
                #[cfg(feature = "rev0-board")]
                {
                    user_output_enabled = false;
                    USER_OUTPUT_REQUEST.reset();
                }
                #[cfg(feature = "rev0-board")]
                logln!("Rev0 OPA1/PB1 raw=0 qualified=0 pd=0 PB10=0; edge-race cutoff");
            }
        } else {
            load_enable.set_low();
        }

        #[cfg(feature = "rev0-board")]
        {
            let load_enabled = load_enable.is_set_high();
            if load_enabled != reported_load_enabled {
                reported_load_enabled = load_enabled;
                logln!(
                    "Rev0 load state: PB10={} raw={} qualified={} managed={} pd={} user={}",
                    u8::from(load_enabled),
                    u8::from(vbus_present.is_high()),
                    u8::from(vbus_is_present()),
                    u8::from(pd_policy_active && vbus_present.is_high()),
                    u8::from(pd_load_permitted && vbus_present.is_high()),
                    u8::from(user_output_enabled),
                );
            }
        }
    }
}

#[cfg(not(any(feature = "usb-control", feature = "dev-text-console")))]
bind_interrupts!(
    struct Irq {
        USBPD => InterruptHandler<peripherals::USBPD>;
    }
);

#[cfg(any(feature = "usb-control", feature = "dev-text-console"))]
bind_interrupts!(
    struct Irq {
        USBPD => InterruptHandler<peripherals::USBPD>;
        USBFS => UsbFsInterruptHandler;
    }
);

fn discard_pending_commands() {
    while COMMANDS.try_receive().is_ok() {}
}

#[cfg(any(feature = "dev-text-console", feature = "sdi-log"))]
fn log_device_identity() {
    // CH32X035 production silicon exposes the same first eight UID bytes as
    // the WCH USB bootloader. Although the reference manual describes a
    // 96-bit ESIG, the final word reads as erased (0xffff_ffff) on observed
    // X035 parts. Do not turn that unprogrammed word into the board label.
    let Some(id) = programmed_device_uid() else {
        logln!("Device id=unavailable");
        return;
    };

    let word0 = u32::from_be_bytes([id[0], id[1], id[2], id[3]]);
    let word1 = u32::from_be_bytes([id[4], id[5], id[6], id[7]]);
    logln!("Device id={:08x}{:08x}", word0, word1);
}

#[cfg(feature = "dev-text-console")]
fn log_console_help() {}

#[cfg(feature = "dev-text-console")]
async fn dev_text_console_rx(mut receiver: CdcReceiver<'static>) -> ! {
    let mut packet = [0u8; 64];
    let mut command = [0u8; CONSOLE_LINE_CAPACITY];
    let mut command_len = 0usize;
    let mut overflowed = false;

    loop {
        receiver.wait_connection().await;
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
                            logln!("Invalid");
                        } else if command_len != 0 {
                            match core::str::from_utf8(&command[..command_len])
                                .ok()
                                .and_then(|line| pd_sink::parse_command(line).ok())
                            {
                                Some(Command::Help) => log_console_help(),
                                Some(Command::Identity) => log_device_identity(),
                                Some(Command::OutputOn) => set_user_output_enabled(true),
                                Some(Command::OutputOff) => set_user_output_enabled(false),
                                Some(parsed) => match COMMANDS.try_send(parsed) {
                                    Ok(()) => {}
                                    Err(_) => logln!("Busy"),
                                },
                                None => logln!("Invalid"),
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

#[cfg(feature = "dev-text-console")]
async fn dev_text_console_tx(mut sender: CdcSender<'static>) -> ! {
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

#[cfg(not(feature = "dev-text-console"))]
fn validity_name(validity: PdoValidity) -> &'static str {
    match validity {
        PdoValidity::Valid => "valid",
        PdoValidity::Compatible => "compatible",
        PdoValidity::ZeroPadding => "padding",
        PdoValidity::Malformed(_) => "malformed",
        PdoValidity::Unsupported => "unsupported",
    }
}

fn log_controller_error(error: ControllerError) {
    let (reason, detail, extra) = match error {
        ControllerError::NoCapabilities(kind) => ("no-caps", kind as u32, 0),
        ControllerError::Busy(state) => ("epr-busy", state as u32, 0),
        ControllerError::EprUnavailable => ("epr-unavailable", 0, 0),
        ControllerError::EprNotConfigured => ("epr-not-configured", 0, 0),
        ControllerError::InvalidEprOperationalPdp(pdp) => ("epr-pdp", pdp.get(), 0),
        ControllerError::NotInEprMode => ("not-in-epr", 0, 0),
        ControllerError::EprExitRefused(reason) => ("epr-exit-refused", reason as u32, 0),
        ControllerError::EprEntryRefused(reason) => ("epr-entry-refused", reason as u32, 0),
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

#[cfg(feature = "dev-text-console")]
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
        logln!("PDO{} raw={:#010x}", pdo.position, pdo.raw);
    }
}

#[cfg(not(feature = "dev-text-console"))]
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
            SourceSupply::EprAvs(avs) => {
                if let Some((standard_minimum, standard_maximum)) = avs.standard_voltage_range() {
                    logln!(
                        "PDO{} EPR-AVS {}-{}mV standard={}-{}mV PDP={}mW peak={} {} raw={:#010x}",
                        pdo.position,
                        avs.min_voltage.get(),
                        avs.max_voltage.get(),
                        standard_minimum.get(),
                        standard_maximum.get(),
                        avs.pdp.get(),
                        avs.peak_current,
                        validity,
                        pdo.raw
                    )
                } else {
                    logln!(
                        "PDO{} EPR-AVS {}-{}mV standard=none PDP={}mW peak={} {} raw={:#010x}",
                        pdo.position,
                        avs.min_voltage.get(),
                        avs.max_voltage.get(),
                        avs.pdp.get(),
                        avs.peak_current,
                        validity,
                        pdo.raw
                    )
                }
            }
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

struct FirmwareSessionTimer;

impl Ch32x035SessionTimer for FirmwareSessionTimer {
    fn now_128ms_ticks() -> u32 {
        (Instant::now().as_ticks() >> 17) as u32
    }

    async fn after_millis(milliseconds: u64) {
        Timer::after_millis(milliseconds).await;
    }
}

#[derive(Clone, Copy)]
struct FirmwarePort;

impl Ch32x035Port for FirmwarePort {
    #[inline(always)]
    fn vbus_present(&self) -> bool {
        vbus_is_present()
    }

    async fn wait_for_vbus_present(&self) {
        VBUS_ATTACHED.wait().await;
    }

    async fn wait_for_vbus_absent(&self) {
        VBUS_DETACHED.wait().await;
    }

    fn begin_session(&self) {
        // A low pulse from a completed Hard Reset or an earlier physical
        // detach belongs to the old, already-invalidated session. Do not let
        // that stored signal immediately cancel the first receive of this
        // fresh startup.
        VBUS_DETACHED.reset();
    }

    #[inline(always)]
    fn set_pd_load_permitted(&self, permitted: bool) {
        set_pd_load_permitted(permitted);
    }

    #[inline(always)]
    fn observe_phy(&self, event: PhyEvent) {
        match event {
            PhyEvent::Attached => {
                control_event!(ControlEvent::Lifecycle { event: ControlLifecycleEvent::Attached, detail: 0, extra: 0 });
                logln!("Attached; PD starts at 5 V");
            }
            PhyEvent::SinkTxAllowed => {
                control_event!(ControlEvent::Lifecycle {
                    event: ControlLifecycleEvent::SinkTxAllowed,
                    detail: 0,
                    extra: 0,
                });
                #[cfg(not(feature = "dev-text-console"))]
                logln!("SinkTxOK; resume");
            }
            PhyEvent::SinkTxDeferred => {
                control_event!(ControlEvent::Lifecycle {
                    event: ControlLifecycleEvent::SinkTxDeferred,
                    detail: 0,
                    extra: 0,
                });
                #[cfg(not(feature = "dev-text-console"))]
                logln!("SinkTxNG; deferred");
            }
        }
    }

    fn observe_session(&self, event: SinkSessionEvent) {
        match event {
            SinkSessionEvent::PhyResetFailed { retry_ms } => {
                control_event!(ControlEvent::Lifecycle {
                    event: ControlLifecycleEvent::PdResetFailed,
                    detail: retry_ms,
                    extra: 0,
                });
                logln!("PD reset failed; retry={}ms", retry_ms);
            }
            SinkSessionEvent::CcDetected => {
                control_event!(ControlEvent::Lifecycle {
                    event: ControlLifecycleEvent::CcDetected,
                    detail: 0,
                    extra: 0,
                });
                logln!("CC; wait VBUS");
            }
            SinkSessionEvent::Recovering { reason, retry_ms, wait_for_detach: _ } => match reason {
                SinkSessionRecovery::Detached => {
                    set_pd_load_unmanaged();
                    control_event!(ControlEvent::Lifecycle {
                        event: ControlLifecycleEvent::PdStoppedDetach,
                        detail: retry_ms,
                        extra: 0,
                    });
                    logln!("PD stopped: detach; retry={}ms", retry_ms);
                }
                SinkSessionRecovery::PhyUnstable => {
                    control_event!(ControlEvent::Lifecycle {
                        event: ControlLifecycleEvent::PdStoppedPhy,
                        detail: retry_ms,
                        extra: 0,
                    });
                    logln!("PD stopped: PHY; retry={}ms", retry_ms);
                }
                SinkSessionRecovery::PortPartnerUnresponsive => {
                    set_pd_load_unmanaged();
                    control_event!(ControlEvent::Lifecycle {
                        event: ControlLifecycleEvent::PdStoppedTimeout,
                        detail: retry_ms,
                        extra: 0,
                    });
                    logln!("PD stopped: timeout; passive retry={}ms", retry_ms);
                }
                SinkSessionRecovery::Protocol => {
                    control_event!(ControlEvent::Lifecycle {
                        event: ControlLifecycleEvent::PdStoppedProtocol,
                        detail: retry_ms,
                        extra: 0,
                    });
                    logln!("PD stopped: protocol; retry={}ms", retry_ms);
                }
                _ => logln!("PD stopped: recovery; retry={}ms", retry_ms),
            },
            SinkSessionEvent::Terminal(error) => match error {
                SinkSessionTerminalError::UnexpectedStop => {
                    control_event!(ControlEvent::Lifecycle {
                        event: ControlLifecycleEvent::PdStopped,
                        detail: 0,
                        extra: 0,
                    });
                    logln!("PD stopped unexpectedly; off");
                }
                SinkSessionTerminalError::LocalPolicy(_) => {
                    control_event!(ControlEvent::Lifecycle {
                        event: ControlLifecycleEvent::PdStoppedPolicy,
                        detail: 0,
                        extra: 0,
                    });
                    logln!("PD stopped: local policy error; off");
                }
                _ => logln!("PD stopped: terminal error; off"),
            },
            _ => logln!("PD session lifecycle event"),
        }
    }
}

struct FirmwareRuntime;

impl SinkRuntime for FirmwareRuntime {
    #[inline(always)]
    fn set_pd_load_permitted(&mut self, permitted: bool) {
        set_pd_load_permitted(permitted);
    }

    #[inline(always)]
    fn set_user_output_enabled(&mut self, enabled: bool) {
        set_user_output_enabled(enabled);
    }

    #[inline(always)]
    fn apply_transition_load_policy(&mut self, policy: TransitionLoadPolicy, transition: ContractTransition) {
        apply_transition_load_policy(policy, transition);
    }

    #[inline(always)]
    fn clear_pending_commands(&mut self) {
        discard_pending_commands();
    }

    #[inline(always)]
    fn capability_plans_enabled(&self) -> bool {
        !cfg!(feature = "dev-text-console")
    }

    async fn wait_for_command(&mut self) -> Command {
        COMMANDS.receive().await
    }

    async fn delay_millis(&mut self, milliseconds: u64) {
        Timer::after_millis(milliseconds).await;
    }

    fn on_source_capabilities(&mut self, capabilities: ProductSourceCapabilities) {
        control_event!(ControlEvent::Capabilities(capabilities));
        log_product_capabilities(&capabilities);
    }

    #[cfg(not(feature = "dev-text-console"))]
    fn on_capability_plans_started(&mut self, count: u8) {
        control_event!(ControlEvent::CapabilityPlansStarted { count });
        logln!("Capability plans: count={} (live contract unchanged)", count);
    }

    fn on_capability_plans_unavailable(&mut self) {
        control_event!(ControlEvent::IntegrationError(ControlIntegrationError::CapabilityPlansUnavailable));
        logln!("Plans unavailable");
    }

    fn on_contract_transition_started(&mut self, transition: ContractTransition) {
        control_event!(ControlEvent::ContractTransition(transition));
        logln!(
            "Transition={} {}mV/{}mA -> {}mV/{}mA",
            transition.kind as u8,
            transition.from.map_or(0, |point| point.voltage.get()),
            transition.from.map_or(0, |point| point.current.get()),
            transition.to.voltage.get(),
            transition.to.current.get()
        );
    }

    #[cfg(not(feature = "dev-text-console"))]
    fn on_capability_plan(&mut self, plan: CapabilityPlan) {
        match plan {
            CapabilityPlan::Unavailable { position, validity } => {
                control_event!(ControlEvent::CapabilityPlanUnavailable { position, validity });
                logln!("Plan PDO{} unavailable {}", position, validity_name(validity))
            }
            CapabilityPlan::Ready(plan) => {
                control_event!(ControlEvent::Plan { stage: ControlPlanStage::Preview, plan: Some(plan) });
                log_request_plan("Plan", plan);
            }
            CapabilityPlan::Rejected(error) => {
                control_event!(ControlEvent::ControllerError(error));
                log_controller_error(error);
            }
        }
    }

    fn on_requesting(&mut self, plan: RequestPlan) {
        control_event!(ControlEvent::Plan { stage: ControlPlanStage::Requesting, plan: Some(plan) });
        log_request_plan("Requesting", plan);
        #[cfg(feature = "sdi-log")]
        logln!("RDO={:#010x}", plan.rdo);
    }

    fn on_contract_ready(&mut self, plan: Option<RequestPlan>) {
        control_event!(ControlEvent::Plan { stage: ControlPlanStage::Contract, plan });
        if let Some(plan) = plan {
            log_request_plan("Contract ready", plan);
        } else {
            logln!("No confirmed contract");
        }
    }

    fn on_contract_refresh_started(&mut self, _plan: RequestPlan) {}

    fn on_contract_refreshed(&mut self, _plan: RequestPlan) {
        control_event!(ControlEvent::Plan { stage: ControlPlanStage::Refreshed, plan: Some(_plan) });
        #[cfg(any(feature = "dev-text-console", feature = "sdi-log"))]
        logln!("Contract refresh confirmed");
    }

    fn on_controller_rejected(&mut self, error: ControllerError) {
        control_event!(ControlEvent::ControllerError(error));
        log_controller_error(error);
    }

    fn on_stack_capabilities_rejected(&mut self, _error: pd_sink::CapabilityListError) {
        control_event!(ControlEvent::IntegrationError(ControlIntegrationError::CapabilitiesRejected));
        logln!("Source caps error");
    }

    fn on_stack_request_rejected(&mut self, _error: pd_sink::StackConversionError) {
        control_event!(ControlEvent::IntegrationError(ControlIntegrationError::RequestRejected));
        logln!("Request error");
    }

    fn on_source_info(&mut self, present_watts: u8, maximum_watts: u8, reported_watts: u8) {
        control_event!(ControlEvent::SourceInfo { present_watts, maximum_watts, reported_watts });
        logln!("Source_Info: present={} W, maximum={} W, reported={} W", present_watts, maximum_watts, reported_watts);
    }

    fn on_source_alert(&mut self, alert: SourceAlert) {
        control_event!(ControlEvent::SourceAlert(alert));
        logln!("Alert: raw={:#010x}", alert.raw());
    }

    fn on_source_status(&mut self, status: SourceStatus) {
        control_event!(ControlEvent::SourceStatus(status));
        let raw = status.raw_bytes();
        logln!(
            "Status: pps={} raw={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            status.pps_mode_valid(),
            raw[0],
            raw[1],
            raw[2],
            raw[3],
            raw[4],
            raw[5],
            raw[6]
        );
    }

    fn on_pps_status(&mut self, status: PpsStatus) {
        control_event!(ControlEvent::PpsStatus(status));
        logln!("PPS_Status: raw={:#010x}", u32::from_le_bytes(status.raw_bytes()));
    }

    fn on_status_query_failed(&mut self, query: StatusQuery, failure: StatusQueryFailure) {
        control_event!(ControlEvent::StatusQueryFailed { query, failure });
        logln!("Status query failed: kind={} reason={}", query as u8, failure as u8);
    }

    fn on_request_result(&mut self, result: RequestResult) {
        control_event!(ControlEvent::RequestResult(result));
        match result {
            RequestResult::Rejected => logln!("Request rejected; old contract active"),
            RequestResult::Deferred => logln!("Request deferred; retry armed"),
        }
    }

    #[cfg(feature = "usb-control")]
    fn on_hard_reset(&mut self, direction: HardResetDirection, _cause: HardResetCause, recovery_ms: u64) {
        control_event!(ControlEvent::HardReset {
            direction,
            cause: _cause,
            recovery_ms: recovery_ms.min(u64::from(u32::MAX)) as u32
        });
        match direction {
            HardResetDirection::Received => logln!("HR received; off; {}ms", recovery_ms),
            HardResetDirection::Sent => logln!("HR sent; off; {}ms", recovery_ms),
        }
    }

    #[cfg(not(feature = "usb-control"))]
    fn on_hard_reset(&mut self, direction: HardResetDirection, recovery_ms: u64) {
        match direction {
            HardResetDirection::Received => logln!("HR received; off; {}ms", recovery_ms),
            HardResetDirection::Sent => logln!("HR sent; off; {}ms", recovery_ms),
        }
    }

    fn on_hard_reset_recovery_complete(&mut self) {
        control_event!(ControlEvent::Lifecycle {
            event: ControlLifecycleEvent::HardResetRecoveryComplete,
            detail: 0,
            extra: 0,
        });
        #[cfg(not(feature = "dev-text-console"))]
        logln!("Reset recovery complete");
    }

    fn on_detached(&mut self) {
        control_event!(ControlEvent::Lifecycle { event: ControlLifecycleEvent::Detached, detail: 0, extra: 0 });
        logln!("Detached; contract lost; off");
    }

    fn on_protocol_lost(&mut self, epr_attempts: u8, maximum_epr_attempts: u8) {
        control_event!(ControlEvent::Lifecycle {
            event: ControlLifecycleEvent::ProtocolLost,
            detail: u32::from(epr_attempts),
            extra: u32::from(maximum_epr_attempts),
        });
        logln!("Protocol lost; off; EPR={}/{}", epr_attempts, maximum_epr_attempts);
    }

    fn on_epr_entry_failed(&mut self, reason: u8) {
        control_event!(ControlEvent::Epr { event: ControlEprEvent::EntryFailed, detail: reason, extra: 0 });
        logln!("EPR failed={}; auto off", reason);
    }

    fn on_epr_discovery_started(&mut self, attempt: u8, maximum_attempts: u8) {
        control_event!(ControlEvent::Epr {
            event: ControlEprEvent::DiscoveryStarted,
            detail: attempt,
            extra: maximum_attempts,
        });
        logln!("EPR enter={}/{}; preserve contract", attempt, maximum_attempts);
    }

    fn on_epr_discovery_unavailable(&mut self) {
        control_event!(ControlEvent::Epr { event: ControlEprEvent::DiscoveryUnavailable, detail: 0, extra: 0 });
        #[cfg(not(feature = "dev-text-console"))]
        logln!("EPR unavailable");
    }

    fn on_epr_automatic_discovery_disabled(&mut self) {
        control_event!(ControlEvent::Epr { event: ControlEprEvent::AutomaticDiscoveryDisabled, detail: 0, extra: 0 });
        logln!("EPR off; SPR active");
    }

    fn on_epr_manual_entry_started(&mut self) {
        control_event!(ControlEvent::Epr { event: ControlEprEvent::ManualEntryStarted, detail: 0, extra: 0 });
        logln!("EPR manual; preserve contract");
    }

    fn on_identity_requested(&mut self) {
        control_event!(ControlEvent::Device(device_info()));
        #[cfg(any(feature = "dev-text-console", feature = "sdi-log"))]
        log_device_identity();
    }

    fn on_help_requested(&mut self) {
        control_event!(ControlEvent::Help);
        #[cfg(feature = "dev-text-console")]
        log_console_help();
    }
}

async fn run_pd(phy: UsbPdPhy<'static, peripherals::USBPD, hal::mode::Async>) -> ! {
    let mut session =
        Ch32x035SinkSession::<_, _, FirmwareSessionTimer>::new(phy, FirmwarePort, sink_config(), FirmwareRuntime)
            .expect("firmware sink configuration must be valid");
    let _ = session.run().await;
    match core::future::pending::<core::convert::Infallible>().await {}
}

#[embassy_executor::main(entry = "qingke_rt::entry")]
async fn main(_spawner: Spawner) {
    #[cfg(feature = "sdi-log")]
    hal::debug::SDIPrint::enable();

    let config = hal::Config {
        rcc: hal::rcc::Config::SYSCLK_FREQ_48MHZ_HSI,
        // This reference does not use general DMA1. USBFS endpoint DMA and
        // USB-PD packet DMA are peripheral-local and remain enabled by their
        // respective drivers.
        enable_dma: false,
        ..Default::default()
    };
    let peripherals = hal::init(config);

    #[cfg(not(feature = "rev0-board"))]
    let vbus_present = ExtiInput::new(peripherals.PA6, peripherals.EXTI6, Pull::Down);
    #[cfg(not(feature = "rev0-board"))]
    let load_enable = Output::new(peripherals.PA7, Level::Low, Speed::Low);
    #[cfg(not(feature = "rev0-board"))]
    let detector_ready = true;

    #[cfg(feature = "rev0-board")]
    let load_enable = Output::new(peripherals.PB10, Level::Low, Speed::Low);
    #[cfg(feature = "rev0-board")]
    let (vbus_present, detector_ready) = rev0_validation::configure_vbus_detector(
        peripherals.OPA,
        peripherals.PC3,
        peripherals.PB4,
        peripherals.PB5,
        peripherals.PB6,
        peripherals.PB1,
        peripherals.EXTI1,
    );

    _spawner.spawn(
        port_supervisor_task(vbus_present, load_enable, detector_ready)
            .expect("port supervisor task allocation failed"),
    );

    let phy = UsbPdPhy::new_async(peripherals.USBPD, peripherals.PC14, peripherals.PC15, Irq);

    #[cfg(not(any(feature = "usb-control", feature = "dev-text-console")))]
    run_pd(phy).await;

    #[cfg(feature = "dev-text-console")]
    {
        let cdc = CdcAcm::new(peripherals.USBFS, peripherals.PC16, peripherals.PC17, Irq);
        let (sender, receiver) = cdc.split();
        join3(dev_text_console_rx(receiver), dev_text_console_tx(sender), run_pd(phy)).await;
    }

    #[cfg(feature = "usb-control")]
    {
        let cdc = CdcAcm::new(peripherals.USBFS, peripherals.PC16, peripherals.PC17, Irq);
        let (sender, receiver) = cdc.split();
        join3(control_transport::receive(receiver), control_transport::transmit(sender), run_pd(phy)).await;
    }
}
