//! D-Bus inhibition with independent display and idle requests. Unlike the
//! keepawake Linux backend, releasing after a service disappears is fallible
//! and must not panic (release builds abort the process on panic).
use super::sleep_inhibitor::InhibitionKind;
use tracing::warn;
use zbus::blocking::{Connection, Proxy};

pub enum KeepAwake {
    Display { connection: Connection, cookie: u32 },
    Idle { _fd: zbus::zvariant::OwnedFd },
}

impl KeepAwake {
    pub fn acquire(kind: InhibitionKind) -> zbus::Result<Self> {
        match kind {
            InhibitionKind::Display => {
                let connection = Connection::session()?;
                let cookie = screensaver(&connection)?
                    .call("Inhibit", &("io.github.x0ra.putmpv", "Video playback"))?;
                Ok(Self::Display { connection, cookie })
            }
            InhibitionKind::Idle => {
                let connection = Connection::system()?;
                let proxy = Proxy::new(
                    &connection,
                    "org.freedesktop.login1",
                    "/org/freedesktop/login1",
                    "org.freedesktop.login1.Manager",
                )?;
                let fd = proxy.call("Inhibit", &("idle", "PutMPV", "Video playback", "block"))?;
                Ok(Self::Idle { _fd: fd })
            }
        }
    }
}

fn screensaver(connection: &Connection) -> zbus::Result<Proxy<'_>> {
    Proxy::new(
        connection,
        "org.freedesktop.ScreenSaver",
        "/org/freedesktop/ScreenSaver",
        "org.freedesktop.ScreenSaver",
    )
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        if let Self::Display { connection, cookie } = self {
            let result: zbus::Result<()> =
                screensaver(connection).and_then(|proxy| proxy.call("UnInhibit", &*cookie));
            if let Err(e) = result {
                warn!("could not release display inhibitor: {e}");
            }
        }
    }
}
