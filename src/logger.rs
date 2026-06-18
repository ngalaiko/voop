use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};

pub struct Logger;

impl log::Log for Logger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        use core::fmt::Write;
        let mut msg: heapless::String<128> = heapless::String::new();
        let _ = write!(msg, "[{}] {}\r\n", record.level(), record.args());
        let _ = LOG_CHANNEL.try_send(msg);
    }

    fn flush(&self) {}
}

static LOG_CHANNEL: Channel<CriticalSectionRawMutex, heapless::String<128>, 32> = Channel::new();

#[embassy_executor::task]
async fn task(mut usb: crate::usb::Usb) {
    loop {
        usb.wait_connection().await;
        loop {
            let msg = LOG_CHANNEL.receive().await;
            for chunk in msg.as_bytes().chunks(64) {
                if usb.write_packet(chunk).await.is_err() {
                    break;
                }
            }
        }
    }
}

static LOGGER: Logger = Logger;

#[derive(Debug)]
pub enum Error {
    SpawnError(embassy_executor::SpawnError),
    SetLoggerError,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::SpawnError(e) => write!(f, "failed to spawn logger task: {:?}", e),
            Error::SetLoggerError => write!(f, "failed to set logger"),
        }
    }
}

impl core::error::Error for Error {}

pub fn init(spawner: embassy_executor::Spawner, usb: crate::usb::Usb) -> Result<(), Error> {
    spawner.spawn(task(usb).map_err(Error::SpawnError)?);
    log::set_logger(&LOGGER).map_err(|_| Error::SetLoggerError)?;
    log::set_max_level(log::LevelFilter::Debug);
    Ok(())
}
