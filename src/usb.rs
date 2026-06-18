use embassy_executor::Spawner;
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::Driver;
use embassy_nrf::{Peri, pac, peripherals};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::driver::EndpointError;
use embassy_usb::UsbDevice;
use static_cell::StaticCell;

type MyDriver = Driver<'static, HardwareVbusDetect>;

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

#[derive(Debug)]
pub enum Error {
    SpawnError(embassy_executor::SpawnError),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::SpawnError(e) => write!(f, "failed to spawn USB task: {:?}", e),
        }
    }
}

impl core::error::Error for Error {}

pub fn init(spawner: Spawner, usbd: Peri<'static, peripherals::USBD>) -> Result<Usb, Error> {
    pac::CLOCK.tasks_hfclkstart().write_value(1);
    while pac::CLOCK.events_hfclkstarted().read() != 1 {}

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
    config.product = Some("Bike Computer");
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
    let usb = builder.build();

    #[embassy_executor::task]
    async fn usb_task(mut device: UsbDevice<'static, MyDriver>) {
        device.run().await;
    }

    spawner.spawn(usb_task(usb).map_err(Error::SpawnError)?);
    Ok(Usb { class })
}
