//! CH32X035G8U6 rev0 validation-board wiring.
//!
//! This module deliberately keeps the board's exact analog and GPIO routing
//! out of the reusable `pd-sink` crate.

use ch32_hal as hal;
use hal::exti::ExtiInput;
use hal::gpio::Pull;
use hal::pac::gpio::vals::{Cnf, Mode};
use hal::{pac, peripherals, Peri};

const OPA_UNLOCK_KEY_1: u32 = 0x4567_0123;
const OPA_UNLOCK_KEY_2: u32 = 0xcdef_89ab;

fn opa_routes_are_fail_closed() -> bool {
    let routes = pac::OPA.cfgr1().read();
    routes.poll_lock()
        && !routes.poll_en1()
        && !routes.poll_en2()
        && !routes.bkin_en1()
        && !routes.bkin_en2()
        && !routes.rst_en1()
        && !routes.rst_en2()
        && !routes.ie_out1()
        && !routes.ie_out2()
        && !routes.ie_cnt()
        && !routes.nmi_en()
}

/// Configures the rev0 active-high minimum-VBUS detector.
///
/// OPA1 compares the divided VBUS signal on PB4 (+) with the approximately
/// 0.42 V reference on PB6 (-), then drives PB5. On the G8U6 package PB5 and
/// PB1 share one physical pad, so PB1 observes the result as a floating input
/// without an external PA3 jumper. PB5's GPIO driver is disabled in analog
/// input mode, OPA1 is the sole pad driver, and PB1 is never driven.
pub fn configure_vbus_detector(
    _opa: Peri<'static, peripherals::OPA>,
    _divider_pc3: Peri<'static, peripherals::PC3>,
    _positive_pb4: Peri<'static, peripherals::PB4>,
    _opa_output_pb5: Peri<'static, peripherals::PB5>,
    _negative_pb6: Peri<'static, peripherals::PB6>,
    sense_pb1: Peri<'static, peripherals::PB1>,
    exti1: Peri<'static, peripherals::EXTI1>,
) -> (ExtiInput<'static>, bool) {
    // PC3 and PB4 are tied to the divider node on rev0. Keep both digital
    // cells high-impedance. PB5's GPIO driver must likewise remain disabled
    // because OPA1 is the sole driver of the PB1/PB5 package pad.
    pac::GPIOC.cfglr().modify(|w| {
        w.set_mode(3, Mode::INPUT);
        w.set_cnf(3, Cnf::ANALOG_IN__PUSH_PULL_OUT);
    });
    pac::GPIOB.cfglr().modify(|w| {
        for pin in 4..=6 {
            w.set_mode(pin, Mode::INPUT);
            w.set_cnf(pin, Cnf::ANALOG_IN__PUSH_PULL_OUT);
        }
    });

    let detector = ExtiInput::new(sense_pb1, exti1, Pull::None);

    // OPA_CFGR1 resets to 0x0080: POLL_LOCK is set and the dedicated
    // active-high OPA1-to-TIM1 brake route is disabled. Deliberately leave
    // that register locked. An unexpected enabled route is an initialization
    // fault, so do not start OPA1 and let the supervisor fail closed.
    if !opa_routes_are_fail_closed() {
        return (detector, false);
    }

    // OPA_CTLR1 resets locked. The official two-key sequence permits the
    // following configuration write.
    pac::OPA.opa_key().write(|w| w.set_opa_key(OPA_UNLOCK_KEY_1));
    pac::OPA.opa_key().write(|w| w.set_opa_key(OPA_UNLOCK_KEY_2));

    pac::OPA.ctlr1().write(|w| {
        w.set_en1(true);
        w.set_mode1(true); // OPA1 output on PB5.
        w.set_psel1(0b10); // OPA1+ on PB4.
        w.set_fb_en1(false);
        w.set_nsel1(0b001); // OPA1- on PB6.
        w.set_en2(false);
        w.set_opa_lock(false);
    });

    // Match WCH's separate OPA_Lock operation rather than relying on
    // unspecified same-write ordering between configuration and the lock.
    pac::OPA.ctlr1().modify(|w| w.set_opa_lock(true));

    let configured = pac::OPA.ctlr1().read();
    let ready = configured.en1()
        && configured.mode1()
        && configured.psel1() == 0b10
        && !configured.fb_en1()
        && configured.nsel1() == 0b001
        && !configured.en2()
        && configured.opa_lock()
        && opa_routes_are_fail_closed();

    (detector, ready)
}
