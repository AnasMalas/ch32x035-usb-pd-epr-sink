//! CH32X035G8U6 flash/PVD backend for the reference black-box profiles.
//!
//! The A/B journal logic and record ABI remain safe Rust. This module is the
//! deliberately small hardware boundary that uses raw flash/PWR registers and
//! SRAM-resident erase/program routines.

use ch32_hal as hal;
use hal::{interrupt, pac};
use pd_sink::black_box::{power_fail_sample_flags, Record, PAGE_SIZE};

const PAGE_A_OFFSET: u32 = 0x0000_f600;
const PAGE_B_OFFSET: u32 = 0x0000_f700;
const PHYSICAL_FLASH_BASE: u32 = 0x0800_0000;

pub struct PvdInterruptHandler;

fn page_offset(index: u8) -> u32 {
    if index == 0 {
        PAGE_A_OFFSET
    } else {
        PAGE_B_OFFSET
    }
}

pub fn read_page(index: u8) -> [u8; PAGE_SIZE] {
    let mut page = [0xff; PAGE_SIZE];
    let offset = page_offset(index);
    // CH32X035 code flash is mapped at address zero for ordinary reads.
    unsafe { core::ptr::copy_nonoverlapping(offset as *const u8, page.as_mut_ptr(), PAGE_SIZE) };
    page
}

pub fn page_is_erased(index: u8) -> bool {
    read_page(index).iter().all(|byte| *byte == 0xff)
}

pub fn erase_page(index: u8) -> bool {
    let address = PHYSICAL_FLASH_BASE + page_offset(index);
    // The page index is bounded by the caller and maps only to the two
    // linker-reserved journal pages.
    unsafe { erase_page_from_ram(address) }
}

pub fn program_page(index: u8, page: &[u8; PAGE_SIZE]) -> bool {
    let address = PHYSICAL_FLASH_BASE + page_offset(index);
    // The page index is bounded by the caller and the target page was erased
    // before PD/PVD were enabled.
    unsafe { program_page_from_ram(address, page) }
}

/// Program one already-erased 256-byte page while executing from SRAM.
#[inline(never)]
#[unsafe(link_section = ".data.ramfunc")]
unsafe fn program_page_from_ram(address: u32, page: &[u8; PAGE_SIZE]) -> bool {
    const FLASH_KEYR: *mut u32 = 0x4002_2004 as *mut u32;
    const FLASH_STATR: *mut u32 = 0x4002_200c as *mut u32;
    const FLASH_CTLR: *mut u32 = 0x4002_2010 as *mut u32;
    const FLASH_ADDR: *mut u32 = 0x4002_2014 as *mut u32;
    const FLASH_MODEKEYR: *mut u32 = 0x4002_2024 as *mut u32;
    const BSY: u32 = 1 << 0;
    const WRPRTERR: u32 = 1 << 4;
    const EOP: u32 = 1 << 5;
    const STRT: u32 = 1 << 6;
    const LOCK: u32 = 1 << 7;
    const FLOCK: u32 = 1 << 15;
    const FTPG: u32 = 1 << 16;
    const BUFLOAD: u32 = 1 << 18;
    const BUFRST: u32 = 1 << 19;

    while unsafe { core::ptr::read_volatile(FLASH_STATR) } & BSY != 0 {}
    unsafe { core::ptr::write_volatile(FLASH_STATR, WRPRTERR) };

    let mut control = unsafe { core::ptr::read_volatile(FLASH_CTLR) };
    if control & LOCK != 0 {
        unsafe {
            core::ptr::write_volatile(FLASH_KEYR, 0x4567_0123);
            core::ptr::write_volatile(FLASH_KEYR, 0xcdef_89ab);
        }
    }
    control = unsafe { core::ptr::read_volatile(FLASH_CTLR) };
    if control & FLOCK != 0 {
        unsafe {
            core::ptr::write_volatile(FLASH_MODEKEYR, 0x4567_0123);
            core::ptr::write_volatile(FLASH_MODEKEYR, 0xcdef_89ab);
        }
    }
    control = unsafe { core::ptr::read_volatile(FLASH_CTLR) } | FTPG;
    unsafe { core::ptr::write_volatile(FLASH_CTLR, control) };
    control = unsafe { core::ptr::read_volatile(FLASH_CTLR) } | BUFRST;
    unsafe { core::ptr::write_volatile(FLASH_CTLR, control) };
    while unsafe { core::ptr::read_volatile(FLASH_STATR) } & BSY != 0 {}
    unsafe { core::ptr::write_volatile(FLASH_STATR, EOP) };

    let source = page.as_ptr();
    let mut word = 0usize;
    while word < PAGE_SIZE / core::mem::size_of::<u32>() {
        let value = unsafe { core::ptr::read_unaligned(source.add(word * 4).cast::<u32>()) };
        unsafe { core::ptr::write_volatile((address as *mut u32).add(word), value) };
        control = unsafe { core::ptr::read_volatile(FLASH_CTLR) } | BUFLOAD;
        unsafe { core::ptr::write_volatile(FLASH_CTLR, control) };
        while unsafe { core::ptr::read_volatile(FLASH_STATR) } & BSY != 0 {}
        if unsafe { core::ptr::read_volatile(FLASH_STATR) } & WRPRTERR != 0 {
            control = unsafe { core::ptr::read_volatile(FLASH_CTLR) } & !FTPG;
            unsafe { core::ptr::write_volatile(FLASH_CTLR, control | LOCK | FLOCK) };
            return false;
        }
        word += 1;
    }

    unsafe { core::ptr::write_volatile(FLASH_ADDR, address) };
    control = unsafe { core::ptr::read_volatile(FLASH_CTLR) } | STRT;
    unsafe { core::ptr::write_volatile(FLASH_CTLR, control) };
    while unsafe { core::ptr::read_volatile(FLASH_STATR) } & BSY != 0 {}
    unsafe { core::ptr::write_volatile(FLASH_STATR, EOP) };
    let ok = unsafe { core::ptr::read_volatile(FLASH_STATR) } & WRPRTERR == 0;
    control = unsafe { core::ptr::read_volatile(FLASH_CTLR) } & !FTPG;
    unsafe { core::ptr::write_volatile(FLASH_CTLR, control | LOCK | FLOCK) };
    ok
}

/// Erase one inactive 256-byte page before PD, USB, or PVD is enabled.
#[inline(never)]
#[unsafe(link_section = ".data.ramfunc")]
unsafe fn erase_page_from_ram(address: u32) -> bool {
    const FLASH_KEYR: *mut u32 = 0x4002_2004 as *mut u32;
    const FLASH_STATR: *mut u32 = 0x4002_200c as *mut u32;
    const FLASH_CTLR: *mut u32 = 0x4002_2010 as *mut u32;
    const FLASH_ADDR: *mut u32 = 0x4002_2014 as *mut u32;
    const FLASH_MODEKEYR: *mut u32 = 0x4002_2024 as *mut u32;
    const BSY: u32 = 1 << 0;
    const WRPRTERR: u32 = 1 << 4;
    const EOP: u32 = 1 << 5;
    const STRT: u32 = 1 << 6;
    const LOCK: u32 = 1 << 7;
    const FLOCK: u32 = 1 << 15;
    const OPTER: u32 = 1 << 5;
    const FTER: u32 = 1 << 17;
    const ERASE_TIMEOUT: u32 = 0x000b_0000;

    let previous_mstatus: usize;
    unsafe {
        core::arch::asm!(
            "csrrci {saved}, mstatus, 8",
            saved = out(reg) previous_mstatus,
            options(nomem, nostack)
        );
    }

    let mut timeout = ERASE_TIMEOUT;
    while unsafe { core::ptr::read_volatile(FLASH_STATR) } & BSY != 0 && timeout != 0 {
        timeout -= 1;
    }

    if timeout != 0 {
        let mut control = unsafe { core::ptr::read_volatile(FLASH_CTLR) };
        if control & LOCK != 0 {
            unsafe {
                core::ptr::write_volatile(FLASH_KEYR, 0x4567_0123);
                core::ptr::write_volatile(FLASH_KEYR, 0xcdef_89ab);
            }
        }
        control = unsafe { core::ptr::read_volatile(FLASH_CTLR) };
        if control & FLOCK != 0 {
            unsafe {
                core::ptr::write_volatile(FLASH_MODEKEYR, 0x4567_0123);
                core::ptr::write_volatile(FLASH_MODEKEYR, 0xcdef_89ab);
            }
        }

        unsafe { core::ptr::write_volatile(FLASH_STATR, EOP | WRPRTERR) };
        control = unsafe { core::ptr::read_volatile(FLASH_CTLR) } & !(OPTER | FTER);
        unsafe {
            core::ptr::write_volatile(FLASH_CTLR, control | FTER);
            core::ptr::write_volatile(FLASH_ADDR, address);
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        control = unsafe { core::ptr::read_volatile(FLASH_CTLR) } | STRT;
        unsafe { core::ptr::write_volatile(FLASH_CTLR, control) };

        timeout = ERASE_TIMEOUT;
        while unsafe { core::ptr::read_volatile(FLASH_STATR) } & BSY != 0 && timeout != 0 {
            timeout -= 1;
        }
    }

    let status = unsafe { core::ptr::read_volatile(FLASH_STATR) };
    let mut control = unsafe { core::ptr::read_volatile(FLASH_CTLR) } & !FTER;
    unsafe {
        core::ptr::write_volatile(FLASH_CTLR, control);
        core::ptr::write_volatile(FLASH_STATR, EOP | WRPRTERR);
    }
    control = unsafe { core::ptr::read_volatile(FLASH_CTLR) };
    unsafe { core::ptr::write_volatile(FLASH_CTLR, control | LOCK | FLOCK) };

    if previous_mstatus & (1 << 3) != 0 {
        unsafe { core::arch::asm!("csrsi mstatus, 8", options(nomem, nostack)) };
    }
    timeout != 0 && status & (BSY | WRPRTERR) == 0
}

pub fn configure_power_fail_detector() {
    use hal::interrupt::typelevel::Interrupt as _;

    // The public rev0 diagnostic board runs VDD at 5 V. Select the 4.0 V PVD
    // threshold so a falling rail leaves time to program one prepared page.
    pac::RCC.apb1pcenr().modify(|w| w.set_pwren(true));
    let pwr_ctlr = 0x4000_7000 as *mut u32;
    let control = unsafe { core::ptr::read_volatile(pwr_ctlr) };
    unsafe { core::ptr::write_volatile(pwr_ctlr, (control & !(0b11 << 5)) | (0b11 << 5) | (1 << 4)) };

    pac::EXTI.intenr().modify(|w| w.set_mr(26, false));
    pac::EXTI.rtenr().modify(|w| w.set_tr(26, true));
    pac::EXTI.ftenr().modify(|w| w.set_tr(26, false));
    pac::EXTI.intfr().write(|w| w.set_if_(26, true));
    pac::EXTI.intenr().modify(|w| w.set_mr(26, true));
    critical_section::with(|cs| {
        interrupt::typelevel::PVD::set_priority_with_cs(cs, interrupt::Priority::P15);
        interrupt::typelevel::PVD::unpend();
        unsafe { interrupt::typelevel::PVD::enable() };
    });
}

impl interrupt::typelevel::Handler<interrupt::typelevel::PVD> for PvdInterruptHandler {
    unsafe fn on_interrupt() {
        pac::EXTI.intfr().write(|w| w.set_if_(26, true));
        let below_threshold = unsafe { core::ptr::read_volatile(0x4000_7004 as *const u32) } & (1 << 2) != 0;
        if !below_threshold {
            return;
        }

        // Cut the active-high reference load request before any journal work.
        pac::GPIOB.bcr().write(|w| w.set_br(10, true));

        // Sample the raw board detector and its interrupt handoff immediately
        // after the safety action. EXTI1 enabled=0 with no pending bit can mean
        // its ISR already dispatched the edge but the executor did not yet run.
        let gpio_b_input = pac::GPIOB.indr().read().0;
        let exti_pending = pac::EXTI.intfr().read().0;
        let exti_enabled = pac::EXTI.intenr().read().0;
        let qualified_present = crate::vbus_is_present();
        let mut sample_flags = 0;
        if gpio_b_input & (1 << 1) == 0 {
            sample_flags |= power_fail_sample_flags::DETECTOR_LOW;
        }
        if exti_pending & (1 << 1) != 0 {
            sample_flags |= power_fail_sample_flags::DETECTOR_EXTI_PENDING;
        }
        if exti_enabled & (1 << 1) != 0 {
            sample_flags |= power_fail_sample_flags::DETECTOR_EXTI_ARMED;
        }
        if qualified_present {
            sample_flags |= power_fail_sample_flags::VBUS_QUALIFIED_PRESENT;
        }
        let sample_context = (gpio_b_input & 0xffff) | ((exti_pending & 0xffff) << 16);

        crate::black_box::latch_power_fail();
        crate::black_box::persist_on_power_fail(sample_flags, sample_context);
    }
}

const _: () = assert!(core::mem::size_of::<Record>() == 12);
