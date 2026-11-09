//! Forward a local TCP port to a TCP port on a USB-connected iPhone through
//! usbmuxd - the same mechanism libimobiledevice's `iproxy` uses. This lets
//! services listening on the phone (e.g. the AgentInput keyboard's WebSocket
//! on port 8787) be reached from the computer over the USB cable alone, with
//! no shared WiFi/LAN.

use idevice::usbmuxd::UsbmuxdConnection;
use std::sync::Arc;
use tokio::sync::Notify;

/// A running localhost forward. Dropping it stops the listener.
pub struct UsbForward {
    pub local_port: u16,
    shutdown: Arc<Notify>,
}

impl UsbForward {
    /// Start forwarding `127.0.0.1:<ephemeral>` -> `<device_port>` on the
    /// iPhone identified by `udid`. The device side is dialed through
    /// usbmuxd per accepted client, so re-dials (the AgentInput keyboard
    /// closing and reopening) each get a fresh connection.
    pub fn start(udid: &str, device_port: u16) -> Result<Self, String> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("bind local forward port: {e}"))?;
        let local_port = listener
            .local_addr()
            .map_err(|e| format!("local forward addr: {e}"))?
            .port();
        // tokio's from_std requires a non-blocking socket - with a blocking
        // one the I/O driver never reports readiness, the accept loop never
        // fires, and clients hang until timeout even though the OS happily
        // completes their TCP handshake into the backlog.
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("nonblocking forward listener: {e}"))?;
        let listener = tokio::net::TcpListener::from_std(listener)
            .map_err(|e| format!("async listener: {e}"))?;

        let shutdown = Arc::new(Notify::new());
        let cancel = shutdown.clone();
        let udid = udid.to_string();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.notified() => break,
                    accepted = listener.accept() => {
                        let (mut client, _) = match accepted {
                            Ok(x) => x,
                            Err(_) => continue,
                        };
                        let udid = udid.clone();
                        tokio::spawn(async move {
                            let t = std::time::Instant::now();
                            match connect_device_port(&udid, device_port).await {
                                Ok(mut device) => {
                                    eprintln!(
                                        "[usb-forward] connected {udid}:{device_port} in {:?}",
                                        t.elapsed()
                                    );
                                    // Pipe until either side closes.
                                    let _ =
                                        tokio::io::copy_bidirectional(&mut client, &mut device)
                                            .await;
                                    eprintln!("[usb-forward] connection closed");
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[usb-forward] {udid}:{device_port} unreachable in {:?}: {e:?}",
                                        t.elapsed(),
                                    );
                                    // Drain whatever the client already sent
                                    // before closing, so it sees a clean EOF
                                    // instead of a reset/timeout (Windows
                                    // closes with RST when data is pending).
                                    use tokio::io::AsyncReadExt;
                                    let mut scratch = [0u8; 4096];
                                    let _ = client.read(&mut scratch).await;
                                }
                            }
                        });
                    }
                }
            }
            eprintln!(
                "[usb-forward] stopped (was 127.0.0.1:{local_port} -> {udid}:{device_port})"
            );
        });

        Ok(Self {
            local_port,
            shutdown,
        })
    }
}

impl Drop for UsbForward {
    fn drop(&mut self) {
        self.shutdown.notify_one();
    }
}

/// Open one connection to `port` on the phone via usbmuxd (like
/// `iproxy <local> <port>` does under the hood).
async fn connect_device_port(
    udid: &str,
    port: u16,
) -> Result<Box<dyn idevice::ReadWrite>, String> {
    let mut conn = UsbmuxdConnection::default()
        .await
        .map_err(|e| format!("usbmuxd connect: {e:?}"))?;
    let device = conn
        .get_device(udid)
        .await
        .map_err(|e| format!("device {udid} not visible to usbmuxd: {e:?}"))?;
    conn.connect_to_device(device.device_id, port, "XPhoneUse")
        .await
        .map_err(|e| {
            format!(
                "usbmuxd forward to port {port} refused: {e:?} \
                 (is the service running on the phone?)"
            )
        })?
        .get_socket()
        .ok_or_else(|| "usbmuxd returned an empty socket".to_string())
}
