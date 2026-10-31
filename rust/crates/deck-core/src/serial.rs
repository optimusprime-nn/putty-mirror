//! Serial sessions, using PuTTY's `Serial*` settings.

use crate::config::Target;
use crate::session::{Command, Event, Io};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::{DataBits, FlowControl, Parity, SerialPortBuilderExt, StopBits};

pub(crate) async fn run(target: &Target, mut io: Io) -> anyhow::Result<String> {
    let s = &target.settings;
    let parity = match s.i("SerialParity", 0) {
        1 => Parity::Odd,
        2 => Parity::Even,
        _ => Parity::None,
    };
    let data = match s.i("SerialDataBits", 8) {
        5 => DataBits::Five,
        6 => DataBits::Six,
        7 => DataBits::Seven,
        _ => DataBits::Eight,
    };
    // stored in half bits: 2 is one stop bit, 4 is two
    let stop = if s.i("SerialStopHalfbits", 2) > 2 { StopBits::Two } else { StopBits::One };
    let flow = match s.i("SerialFlowControl", 1) {
        0 => FlowControl::None,
        2 => FlowControl::Hardware,
        _ => FlowControl::Software,
    };
    let mut port = tokio_serial::new(target.host.as_str(), s.i("SerialSpeed", 9600))
        .parity(parity)
        .data_bits(data)
        .stop_bits(stop)
        .flow_control(flow)
        .open_native_async()
        .map_err(|e| anyhow::anyhow!("Could not open {}: {e}", target.host))?;
    io.logged_in();
    let mut buf = [0u8; 8192];
    loop {
        tokio::select! {
            n = port.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    return Ok("The serial line closed.".into());
                }
                (io.events)(Event::Output(buf[..n].to_vec()));
            }
            cmd = io.commands.recv() => match cmd {
                None | Some(Command::Close) => return Ok("Session closed.".into()),
                // answers reach here as Input, once the session is ready
                Some(Command::Reply(_)) => {}
                Some(Command::Input(b)) => port.write_all(&b).await?,
                Some(Command::Resize { .. }) => {}
            },
        }
    }
}
