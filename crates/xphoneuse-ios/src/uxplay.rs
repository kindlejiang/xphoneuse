use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::process::{Child, Command};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub(crate) static RESOURCE_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_resource_dir(path: PathBuf) {
    let _ = RESOURCE_DIR.set(path);
}

fn uxplay_binary_name() -> &'static str {
    if cfg!(windows) { "uxplay.exe" } else { "uxplay" }
}

pub fn uxplay_path() -> PathBuf {
    let binary = uxplay_binary_name();

    if let Some(dir) = RESOURCE_DIR.get() {
        let bundled = dir.join("resources").join(binary);
        if bundled.exists() {
            return bundled;
        }
    }
    PathBuf::from(binary)
}

pub struct UxplayProcess {
    pub child: Child,
}

impl UxplayProcess {
    /// Spawn uxplay headlessly, mirroring the AirPlay H.264 stream onto
    /// `udp_port` via RTP. Audio is disabled; no video window is opened.
    pub fn spawn(udp_port: u16, server_name: &str, fps: u32) -> Result<UxplayProcess> {
        let path = uxplay_path();

        let rtp_pipeline = format!(
            "config-interval=1 ! udpsink host=127.0.0.1 port={}",
            udp_port
        );

        let mut cmd = Command::new(&path);
        cmd.args([
            "-n",
            server_name,
            "-fps",
            &fps.to_string(),
            "-as",
            "0",
            "-nohold",
            "-vrtp",
            &rtp_pipeline,
        ]);
        #[cfg(windows)]
        {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let child = cmd
            .spawn()
            .map_err(|e| anyhow!("failed to start uxplay ({}): {}", path.display(), e))?;

        Ok(UxplayProcess { child })
    }

    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}
