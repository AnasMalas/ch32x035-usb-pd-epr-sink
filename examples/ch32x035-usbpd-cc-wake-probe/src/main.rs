#![no_std]
#![no_main]
#![forbid(unsafe_code)]

use ch32_hal as hal;
use ch32_hal::usbpd::{CcWakeInterruptHandler, Error, InterruptHandler, UsbPdPhy};
use ch32_hal::{bind_interrupts, peripherals};
use embassy_executor::Spawner;
use embassy_time::{Instant, Timer};
use panic_halt as _;

bind_interrupts!(
    struct Irq {
        USBPD => InterruptHandler<peripherals::USBPD>;
        USBPD_WKUP => CcWakeInterruptHandler<peripherals::USBPD>;
    }
);

const REATTACH_POLL_MS: u64 = 20;
const SAMPLE_OFFSETS_US: [u64; 8] = [0, 5, 10, 20, 50, 100, 250, 1_000];

#[embassy_executor::main(entry = "qingke_rt::entry")]
async fn main(_spawner: Spawner) {
    hal::debug::SDIPrint::enable();

    let config = hal::Config { rcc: hal::rcc::Config::SYSCLK_FREQ_48MHZ_HSI, enable_dma: false, ..Default::default() };
    let peripherals = hal::init(config);
    let (phy, mut monitor) =
        UsbPdPhy::new_async_with_cc_monitor(peripherals.USBPD, peripherals.PC14, peripherals.PC15, Irq);

    hal::println!("CH32X035 USBPD IE_PD_IO CC wake probe");
    hal::println!("No PD negotiation, VBUS sensing, or load control is active");

    loop {
        hal::println!("Waiting for Source attachment at the 0.22 V CC threshold");
        loop {
            match phy.detect_cc() {
                Ok(()) => break,
                Err(Error::CCNotConnected) => Timer::after_millis(REATTACH_POLL_MS).await,
                Err(error) => {
                    hal::println!("Unexpected CC detection error: {:?}", error);
                    Timer::after_millis(REATTACH_POLL_MS).await;
                }
            }
        }

        let attached_at = Instant::now();
        let first_interrupt = monitor.wake_interrupt_count();
        hal::println!(
            "Attached; active_cc_high={} wake_count={}; waiting for sustained low",
            monitor.active_cc_high(),
            first_interrupt
        );

        monitor.wait_for_active_cc_low().await;

        let observed_at = Instant::now();
        let mut observed_offsets = [0_u64; SAMPLE_OFFSETS_US.len()];
        let mut observed_high = [false; SAMPLE_OFFSETS_US.len()];
        for (index, target_offset) in SAMPLE_OFFSETS_US.iter().copied().enumerate() {
            let elapsed = Instant::now().saturating_duration_since(observed_at).as_micros();
            if target_offset > elapsed {
                Timer::after_micros(target_offset - elapsed).await;
            }
            observed_offsets[index] = Instant::now().saturating_duration_since(observed_at).as_micros();
            observed_high[index] = monitor.active_cc_high();
        }

        let final_interrupt = monitor.wake_interrupt_count();
        hal::println!(
            "Low observed after {} us attached; wake_count={} delta={}",
            observed_at.saturating_duration_since(attached_at).as_micros(),
            final_interrupt,
            final_interrupt.wrapping_sub(first_interrupt)
        );
        for index in 0..SAMPLE_OFFSETS_US.len() {
            hal::println!("  sample={} us active_cc_high={}", observed_offsets[index], observed_high[index]);
        }
    }
}
