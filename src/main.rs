#![no_std]
#![no_main]

use core::fmt::Write;
use embassy_executor::Spawner;
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::Driver;
use embassy_nrf::{bind_interrupts, peripherals, usb};
use embassy_time::Timer;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::UsbDevice;
use heapless::String;
use panic_halt as _;
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
});

type MyDriver = Driver<'static, peripherals::USBD, HardwareVbusDetect>;

#[embassy_executor::task]
async fn usb_task(mut device: UsbDevice<'static, MyDriver>) {
    device.run().await;
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_nrf::init(Default::default());

    let mut red = Output::new(p.P0_26, Level::High, OutputDrive::Standard);
    let mut blue = Output::new(p.P0_06, Level::High, OutputDrive::Standard);

    // USB driver — hands the hardware peripheral to embassy-usb
    let driver = Driver::new(p.USBD, Irqs, HardwareVbusDetect::new(Irqs));

    // Buffers the USB stack needs — must be 'static
    static STATE: StaticCell<State> = StaticCell::new();
    static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static MSOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();

    let state = STATE.init(State::new());
    let config_desc = CONFIG_DESCRIPTOR.init([0u8; 256]);
    let bos_desc = BOS_DESCRIPTOR.init([0u8; 256]);
    let msos_desc = MSOS_DESCRIPTOR.init([0u8; 256]);
    let control_buf = CONTROL_BUF.init([0u8; 64]);

    // Describe the USB device
    let mut config = embassy_usb::Config::new(0xc0de, 0xcafe);
    config.manufacturer = Some("Nikita");
    config.product = Some("Bike Computer");
    config.serial_number = Some("1");
    config.max_power = 100;

    // Build the USB device + attach CDC ACM class
    let mut builder = embassy_usb::Builder::new(
        driver,
        config,
        config_desc,
        bos_desc,
        msos_desc,
        control_buf,
    );
    let mut class = CdcAcmClass::new(&mut builder, state, 64);
    let usb = builder.build();

    // Spawn the USB task — runs the protocol stack forever
    spawner.spawn(usb_task(usb)).unwrap();

    loop {
        // Wait for host to connect (DTR = Data Terminal Ready)
        class.wait_connection().await;
        blue.set_low();

        let mut i = 0;
        loop {
            let mut buf: String<32> = String::new();
            write!(buf, "Hello {}\r\n", i).unwrap();
            match class.write_packet(buf.as_bytes()).await {
                Ok(_) => {}
                Err(_) => break, // host disconnected
            }
            red.set_low();
            Timer::after_millis(500).await;
            red.set_high();
            Timer::after_millis(500).await;
            i += 1;
        }

        blue.set_high();
    }
}
