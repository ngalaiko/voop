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
static LOGGER: Logger = Logger;

pub struct LoggerRunner {
    usb: crate::usb::Usb,
}

impl LoggerRunner {
    pub async fn run(mut self) {
        loop {
            self.usb.wait_connection().await;
            'connected: loop {
                let msg = LOG_CHANNEL.receive().await;
                for chunk in msg.as_bytes().chunks(64) {
                    if self.usb.write_packet(chunk).await.is_err() {
                        // Port gone — wait for the next connection instead of consuming
                        // (and discarding) messages into a dead endpoint.
                        break 'connected;
                    }
                }
                // A message that is an exact multiple of the 64-byte packet size ends on a
                // full packet; follow with a zero-length packet so host CDC stacks flush it
                // instead of buffering until the next log line.
                if !msg.is_empty() && msg.len() % 64 == 0 {
                    let _ = self.usb.write_packet(&[]).await;
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum Error {
    SetLoggerError,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::SetLoggerError => write!(f, "failed to set logger"),
        }
    }
}

impl core::error::Error for Error {}

pub fn init(usb: crate::usb::Usb) -> Result<LoggerRunner, Error> {
    log::set_logger(&LOGGER).map_err(|_| Error::SetLoggerError)?;
    log::set_max_level(log::LevelFilter::Debug);
    Ok(LoggerRunner { usb })
}
