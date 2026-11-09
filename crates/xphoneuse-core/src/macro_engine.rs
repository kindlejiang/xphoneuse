use crate::control;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MacroEvent {
    #[serde(rename = "touch")]
    Touch { action: String, x: f64, y: f64 },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "key")]
    Key { keycode: u32, action: String },
    #[serde(rename = "scroll")]
    Scroll { x: f64, y: f64, dx: f64, dy: f64 },
    #[serde(rename = "button")]
    Button { button: String },
    #[serde(rename = "wait")]
    Wait { ms: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimedEvent {
    pub timestamp_ms: u64,
    pub event: MacroEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Macro {
    pub name: String,
    pub events: Vec<TimedEvent>,
}

pub struct MacroRecorder {
    name: String,
    start_time: std::time::Instant,
    events: Vec<TimedEvent>,
}

impl MacroRecorder {
    pub fn new(name: String) -> Self {
        Self {
            name,
            start_time: std::time::Instant::now(),
            events: Vec::new(),
        }
    }

    pub fn record(&mut self, event: MacroEvent) {
        let elapsed = self.start_time.elapsed().as_millis() as u64;
        self.events.push(TimedEvent {
            timestamp_ms: elapsed,
            event,
        });
    }

    pub fn finish(self) -> Macro {
        Macro {
            name: self.name,
            events: self.events,
        }
    }
}

/// Play `events` back, giving up when `stop` says so.
///
/// `stop` exists for loop playback, which the user ends by hand: without it a
/// press would have to wait out the macro in flight - and a macro can carry a
/// ten-second wait - before anything happened.
pub async fn play_events(
    events: &[TimedEvent],
    socket: &Arc<Mutex<TcpStream>>,
    screen_w: u32,
    screen_h: u32,
    stop: &(dyn Fn() -> bool + Send + Sync),
) -> Result<()> {
    // A stop can land between a touch's down and its up. The device would keep
    // that finger down - and the next touch would read as a second contact -
    // so the position is remembered and released before returning, whatever
    // the reason for returning.
    let mut held: Option<(f64, f64)> = None;
    let result = replay(events, socket, screen_w, screen_h, stop, &mut held).await;
    if let Some((x, y)) = held {
        let px = (x * screen_w as f64) as u32;
        let py = (y * screen_h as f64) as u32;
        let _ = control::inject_touch(socket, "up", px, py, screen_w as u16, screen_h as u16).await;
    }
    result
}

/// Sleep for `ms` unless `stop` is asked for first, checked in slices so a
/// press is noticed within about 50ms even inside a long wait.
///
/// `false` means "stop" - the caller is done.
pub async fn wait_or_stop(ms: u64, stop: &(dyn Fn() -> bool + Send + Sync)) -> bool {
    const SLICE_MS: u64 = 50;
    let mut left = ms;
    while left > 0 {
        if stop() {
            return false;
        }
        let slice = left.min(SLICE_MS);
        tokio::time::sleep(tokio::time::Duration::from_millis(slice)).await;
        left -= slice;
    }
    !stop()
}

async fn replay(
    events: &[TimedEvent],
    socket: &Arc<Mutex<TcpStream>>,
    screen_w: u32,
    screen_h: u32,
    stop: &(dyn Fn() -> bool + Send + Sync),
    held: &mut Option<(f64, f64)>,
) -> Result<()> {
    let mut last_ts = 0u64;

    for ev in events {
        if stop() {
            break;
        }
        let delay = ev.timestamp_ms.saturating_sub(last_ts);
        if delay > 0 && !wait_or_stop(delay, stop).await {
            break;
        }
        last_ts = ev.timestamp_ms;

        match &ev.event {
            MacroEvent::Touch { action, x, y } => {
                *held = if action == "up" { None } else { Some((*x, *y)) };
                let px = (*x * screen_w as f64) as u32;
                let py = (*y * screen_h as f64) as u32;
                control::inject_touch(
                    socket,
                    action,
                    px,
                    py,
                    screen_w as u16,
                    screen_h as u16,
                )
                .await?;
            }
            MacroEvent::Text { text } => {
                control::send_text(socket, text).await?;
            }
            MacroEvent::Key { keycode, action } => {
                control::inject_keycode(socket, action, *keycode, 0, 0).await?;
            }
            MacroEvent::Scroll { x, y, dx, dy } => {
                let px = (*x * screen_w as f64) as u32;
                let py = (*y * screen_h as f64) as u32;
                let sx = (*dx * 120.0) as i16;
                let sy = (*dy * 120.0) as i16;
                control::inject_scroll(
                    socket,
                    px,
                    py,
                    screen_w as u16,
                    screen_h as u16,
                    sx,
                    sy,
                )
                .await?;
            }
            MacroEvent::Button { button } => {
                let kc = match button.as_str() {
                    "home" => control::KEYCODE_HOME,
                    "back" => control::KEYCODE_BACK,
                    "recents" => control::KEYCODE_APP_SWITCH,
                    "power" => control::KEYCODE_POWER,
                    "volume_up" => control::KEYCODE_VOLUME_UP,
                    "volume_down" => control::KEYCODE_VOLUME_DOWN,
                    _ => continue,
                };
                control::inject_keycode(socket, "down", kc, 0, 0).await?;
                control::inject_keycode(socket, "up", kc, 0, 0).await?;
            }
            MacroEvent::Wait { ms } => {
                if !wait_or_stop(*ms, stop).await {
                    break;
                }
            }
        }
    }

    Ok(())
}
