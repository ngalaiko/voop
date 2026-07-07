use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::Driver;
use embassy_nrf::{peripherals, Peri};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::driver::EndpointError;
use embassy_usb::UsbDevice;
use static_cell::StaticCell;

type MyDriver = Driver<'static, HardwareVbusDetect>;

pub struct UsbRunner {
    device: UsbDevice<'static, MyDriver>,
}

impl UsbRunner {
    pub async fn run(mut self) {
        self.device.run().await;
    }
}

pub struct Usb {
    pub(crate) class: CdcAcmClass<'static, MyDriver>,
}

impl Usb {
    pub async fn wait_connection(&mut self) {
        self.class.wait_connection().await;
    }

    pub async fn write_packet(&mut self, data: &[u8]) -> Result<(), EndpointError> {
        self.class.write_packet(data).await
    }
}

// USB needs HFXO running continuously, but MPSL owns the CLOCK peripheral and would stop a
// raw-register-started HFCLK whenever the radio goes idle. main() requests the clock through
// MPSL (Ble::request_hfclk) after BLE init, before the USB device task starts.
pub fn init(usbd: Peri<'static, peripherals::USBD>) -> (UsbRunner, Usb) {
    let driver = Driver::new(usbd, crate::Irqs, HardwareVbusDetect::new(crate::Irqs));

    static STATE: StaticCell<State<'static>> = StaticCell::new();
    static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static MSOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();

    let state = STATE.init(State::new());
    let config_desc = CONFIG_DESCRIPTOR.init([0u8; 256]);
    let bos_desc = BOS_DESCRIPTOR.init([0u8; 256]);
    let msos_desc = MSOS_DESCRIPTOR.init([0u8; 256]);
    let control_buf = CONTROL_BUF.init([0u8; 64]);

    let mut config = embassy_usb::Config::new(0xc0de, 0xcafe);
    config.manufacturer = Some("Nikita");
    config.product = Some("Voop");
    config.serial_number = Some("1");
    config.max_power = 100;

    let mut builder = embassy_usb::Builder::new(
        driver,
        config,
        &mut config_desc[..],
        &mut bos_desc[..],
        &mut msos_desc[..],
        &mut control_buf[..],
    );
    let class = CdcAcmClass::new(&mut builder, state, 64);
    let device = builder.build();

    (UsbRunner { device }, Usb { class })
}
