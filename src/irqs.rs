use embassy_nrf::{bind_interrupts, peripherals, rng, twim, uarte, usb};
use nrf_sdc::mpsl;

bind_interrupts!(pub struct Irqs {
    UARTE0 => uarte::InterruptHandler<peripherals::UARTE0>;
    TWISPI0 => twim::InterruptHandler<peripherals::TWISPI0>;
    USBD => usb::InterruptHandler<peripherals::USBD>;
    CLOCK_POWER => usb::vbus_detect::InterruptHandler, mpsl::ClockInterruptHandler;
    RNG => rng::InterruptHandler<peripherals::RNG>;
    EGU0_SWI0 => mpsl::LowPrioInterruptHandler;
    RADIO => mpsl::HighPrioInterruptHandler;
    TIMER0 => mpsl::HighPrioInterruptHandler;
    RTC0 => mpsl::HighPrioInterruptHandler;
});
