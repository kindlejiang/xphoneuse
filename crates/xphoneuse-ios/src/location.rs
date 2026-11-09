//! Simulating the iPhone's location.
//!
//! Xcode can put a device at an arbitrary position and so can we: the
//! developer image serves an Instruments channel
//! (`com.apple.instruments.server.services.LocationSimulation`) reached
//! exactly like the screenshot and process-control ones (CoreDeviceProxy →
//! RSD → dtservicehub).
//!
//! One thing shapes this module: **the simulation lives as long as the
//! connection does.** `set` is not a setting that persists on the device - it
//! is a message on a channel, and the phone reverts to its real position the
//! moment the channel closes. So a session here is a long-lived actor holding
//! the tunnel, and callers move the position by sending it coordinates.
//!
//! Dropping the handle ends it (the actor's channel closes, the connection
//! goes with it, the phone goes back to GPS) - which is what "stop" means, and
//! it happens by itself when the app exits, since nothing then holds the
//! device open.

use idevice::dvt::location_simulation::LocationSimulationClient;
use idevice::ReadWrite;
use tokio::sync::{mpsc, oneshot};

use crate::screenshot::{build_dvt_server, is_service_missing};

/// How long a set/clear waits for the device to answer. The channel is local
/// and the device answers immediately; this is only here so a wedged
/// connection cannot hang a UI thread forever.
const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

enum Command {
    Set {
        lat: f64,
        lon: f64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Clear {
        reply: oneshot::Sender<Result<(), String>>,
    },
}

/// A live location session. Dropping it stops the simulation.
pub struct LocationSession {
    tx: mpsc::Sender<Command>,
}

impl LocationSession {
    /// Put the device at `lat`,`lon` (WGS-84 - what a GPS reports and what
    /// OpenStreetMap draws).
    pub async fn set(&self, lat: f64, lon: f64) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Set { lat, lon, reply })
            .await
            .map_err(|_| "the location session is no longer running".to_string())?;
        match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("the location session ended mid-request".to_string()),
            Err(_) => Err("the device did not answer the location request".to_string()),
        }
    }

    /// Hand the device back to its real position, keeping the session open for
    /// the next `set`.
    pub async fn clear(&self) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Clear { reply })
            .await
            .map_err(|_| "the location session is no longer running".to_string())?;
        match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("the location session ended mid-request".to_string()),
            Err(_) => Err("the device did not answer the location request".to_string()),
        }
    }
}

/// Open a session on `udid`.
pub async fn start(udid: &str) -> Result<LocationSession, String> {
    let provider = crate::device::provider_for(udid)
        .await
        .map_err(|e| format!("{e:#}"))?;
    // Everything but the location channel itself. The adapter is held by the
    // actor for the session's lifetime: the channel rides on it, and the
    // simulation dies with it.
    let (adapter, server, _rx_bytes) = build_dvt_server(provider.as_ref()).await?;

    let (tx, rx) = mpsc::channel(4);
    let (ready_tx, ready_rx) = oneshot::channel();
    tokio::spawn(run(adapter, server, rx, ready_tx));

    match ready_rx.await {
        Ok(Ok(())) => Ok(LocationSession { tx }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("the location session ended before it opened".to_string()),
    }
}

/// Start, giving the developer image one chance to be prepared first - the
/// channel lives in it, and an iOS update invalidates the mounted one (the
/// same failure the screenshot and launch paths know). Preparing the image is
/// this side's job; turning Developer Mode on is not.
pub async fn start_ready(udid: &str) -> Result<LocationSession, String> {
    match start(udid).await {
        Err(error) if is_service_missing(&error) => {
            crate::developer_image::ensure(udid, crate::developer_image::Route::Personalized)
                .await
                .map_err(|e| format!("{error}; preparing the developer image also failed: {e}"))?;
            start(udid).await
        }
        result => result,
    }
}

/// Owns the connection and answers commands until the handle is dropped.
async fn run(
    _adapter: idevice::tcp::handle::AdapterHandle,
    mut server: idevice::dvt::remote_server::RemoteServerClient<Box<dyn ReadWrite>>,
    mut rx: mpsc::Receiver<Command>,
    ready: oneshot::Sender<Result<(), String>>,
) {
    let mut client = match LocationSimulationClient::new(&mut server).await {
        Ok(client) => {
            let _ = ready.send(Ok(()));
            client
        }
        Err(e) => {
            let _ = ready.send(Err(format!("location channel failed: {e}")));
            return;
        }
    };

    while let Some(command) = rx.recv().await {
        match command {
            Command::Set { lat, lon, reply } => {
                let result = client
                    .set(lat, lon)
                    .await
                    .map_err(|e| format!("could not set the location: {e}"));
                let _ = reply.send(result);
            }
            Command::Clear { reply } => {
                let result = client
                    .clear()
                    .await
                    .map_err(|e| format!("could not clear the location: {e}"));
                let _ = reply.send(result);
            }
        }
    }
    // The handle is gone: drop the client and the connection with it, which is
    // what hands the device back to its real position.
}
