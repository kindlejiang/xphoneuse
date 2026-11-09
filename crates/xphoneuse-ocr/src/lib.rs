//! Offline OCR: PaddleOCR models (PP-OCRv6) running on MNN, through `ocr-rs`.
//!
//! The point of this crate is the iOS side of XPhoneUse. Android can read its
//! screen through the accessibility tree, which is exact and cheap; an iPhone
//! has nothing comparable, so reading text off the screen used to mean sending
//! a 1179x2556 screenshot to a vision model. OCR turns that into a few hundred
//! tokens of plain text.
//!
//! The interface is deliberately narrow - bytes in, text lines out - so the
//! `image` and `imageproc` types that `ocr-rs` speaks never leak into callers,
//! and so the same entry point can later serve the "watch the screen and OCR
//! what changed" flow without going through MCP.
//!
//! Models are not part of the `ocr-rs` crate (or of any crates.io package) and
//! are shipped with the app under `resources/ocr/`; see `models_dir`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ocr_rs::{DetOptions, OcrEngine, OcrEngineConfig};
use serde::Serialize;

/// The three files `models_dir()` must contain. The names are the upstream
/// ones, so a checkout of the ocr-rs repository can be pointed at directly
/// with `XPHONEUSE_OCR_MODELS`.
pub const DET_MODEL: &str = "PP-OCRv6_small_det.mnn";
pub const REC_MODEL: &str = "PP-OCRv6_small_rec.mnn";
pub const CHARSET: &str = "ppocr_keys_v6_small.txt";

/// Longest side the detector sees, in pixels.
///
/// The library's default is 960, which shrinks a 1179x2556 phone screenshot to
/// 443x960 - and small glyphs do not survive that. Measured on a home screen
/// full of app labels (1179x2556, `crates/xphoneuse-ocr/examples/max_side_probe`),
/// lines recognized by max_side_len: 62 at 960, 81 at 1600, 102 at 1920, 108 at
/// 2560 - and 108 again at 3000, because past the image's own long side there
/// is nothing left to downscale. The missing ones at 960 are not noise: they
/// are icon labels like 小红书, 云闪付 and BOSS直聘, all above 0.9 confidence.
///
/// So this is set to (a little over) the long side of a modern phone
/// screenshot, which makes it mean "do not downscale" for the case that
/// matters. Cost is real but small: ~1.2 s per call against ~0.45 s at 960,
/// next to a screenshot that already takes ~0.7 s over the tunnel.
const MAX_SIDE_LEN: u32 = 2560;

/// One recognized line of text.
#[derive(Debug, Clone, Serialize)]
pub struct OcrLine {
    pub text: String,
    pub confidence: f32,
    /// `[x, y, width, height]` in pixels of the image that was passed in.
    /// The detector works on a downscaled copy but maps its boxes back, so
    /// these are source-image coordinates - divide by the image width and
    /// height to get the normalized coordinates `xphoneuse_tap` wants.
    pub bbox: [f32; 4],
}

/// Everything a caller needs to map the boxes back onto the screen it came
/// from, which is why the size travels with the lines.
#[derive(Debug, Clone, Serialize)]
pub struct OcrImage {
    pub width: u32,
    pub height: u32,
    pub lines: Vec<OcrLine>,
}

static RESOURCE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Where the app's bundled files live - the directory that *contains*
/// `resources/`, same as `xphoneuse_core::adb::set_resource_dir`. Called from the
/// app's setup hook; without it, `models_dir` falls back to the executable's
/// own directory.
pub fn set_resource_dir(path: PathBuf) {
    let _ = RESOURCE_DIR.set(path);
}

/// The directory holding the three model files.
///
/// `XPHONEUSE_OCR_MODELS` wins, so a developer can point at the ocr-rs checkout
/// instead of copying 16 MB into the repository. Otherwise it is
/// `<resource_dir>/resources/ocr`, and failing that the same path next to the
/// executable (which is how the standalone `xphoneuse-mcp` binary finds them).
pub fn models_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XPHONEUSE_OCR_MODELS") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Some(dir) = RESOURCE_DIR.get() {
        return dir.join("resources").join("ocr");
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("resources").join("ocr")))
        .unwrap_or_else(|| PathBuf::from("resources/ocr"))
}

/// Recognized lines of a PNG (or any format the `image` crate decodes),
/// in reading order as the detector found them.
pub async fn recognize_png(bytes: Vec<u8>) -> Result<OcrImage, String> {
    let dir = models_dir();
    // Loading a model and running inference are both CPU-bound and blocking;
    // neither belongs on the async runtime's worker threads.
    tokio::task::spawn_blocking(move || recognize_blocking(&bytes, &dir))
        .await
        .map_err(|e| format!("OCR worker failed: {e}"))?
}

fn recognize_blocking(bytes: &[u8], dir: &Path) -> Result<OcrImage, String> {
    let image = image::load_from_memory(bytes).map_err(|e| format!("could not decode the image: {e}"))?;
    let (width, height) = (image.width(), image.height());

    // One engine per process, keyed by the directory it was built from: the
    // app is long-lived, and reloading 16 MB of weights per call would make
    // every OCR slower than the screenshot it came from. The key matters
    // because a process can be started pointed somewhere else - a build tree
    // with `XPHONEUSE_OCR_MODELS`, an app whose resources moved - and the
    // first call after that must load the models it was actually pointed at.
    // A failure for a given directory is cached like any other result, so a
    // bad path costs one attempt rather than one per call.
    let cache = ENGINE.get_or_init(|| {
        Mutex::new(EngineCache {
            dir: dir.to_path_buf(),
            engine: build_engine(dir),
        })
    });
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.dir != dir {
        cache.dir = dir.to_path_buf();
        cache.engine = build_engine(dir);
    }
    let engine = cache.engine.as_ref().map_err(|e| e.clone())?;

    let results = engine.recognize(&image).map_err(|e| format!("OCR failed: {e}"))?;
    let lines = results
        .into_iter()
        .map(|line| {
            let rect = line.bbox.rect;
            OcrLine {
                text: line.text,
                confidence: line.confidence,
                bbox: [
                    rect.left() as f32,
                    rect.top() as f32,
                    rect.width() as f32,
                    rect.height() as f32,
                ],
            }
        })
        .collect();
    Ok(OcrImage { width, height, lines })
}

struct EngineCache {
    dir: PathBuf,
    engine: Result<OcrEngine, String>,
}

static ENGINE: OnceLock<Mutex<EngineCache>> = OnceLock::new();

fn build_engine(dir: &Path) -> Result<OcrEngine, String> {
    let det = dir.join(DET_MODEL);
    let rec = dir.join(REC_MODEL);
    let charset = dir.join(CHARSET);
    for (label, path) in [("detection model", &det), ("recognition model", &rec), ("charset", &charset)] {
        if !path.is_file() {
            return Err(format!(
                "OCR {label} not found: {} (looked in {}; set XPHONEUSE_OCR_MODELS to a directory holding {DET_MODEL}, {REC_MODEL} and {CHARSET})",
                path.display(),
                dir.display()
            ));
        }
    }

    let config = OcrEngineConfig {
        // The engine's own threshold defaults to 0.5 and drops lines before the
        // caller sees them, which would make a lower `min_confidence` a no-op.
        // Filtering happens in the caller instead.
        min_result_confidence: 0.0,
        det_options: DetOptions {
            max_side_len: MAX_SIDE_LEN,
            ..Default::default()
        },
        ..Default::default()
    };
    OcrEngine::new(&det, &rec, &charset, Some(config))
        .map_err(|e| format!("could not load the OCR models from {}: {e}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OCR of a real screenshot, against whatever models `XPHONEUSE_OCR_MODELS`
    /// points at. Skipped (not failed) without them, the same way ocr-rs's own
    /// integration tests behave: the models are 16 MB of weights and are not
    /// in the repository checkout of this crate.
    #[test]
    fn reads_text_from_a_screenshot() {
        let Ok(dir) = std::env::var("XPHONEUSE_OCR_MODELS") else {
            eprintln!("skipped: XPHONEUSE_OCR_MODELS is not set");
            return;
        };
        // The ocr-rs checkout ships test images next to its models.
        let sample = Path::new(&dir)
            .parent()
            .map(|root| root.join("res"))
            .filter(|res| res.is_dir())
            .and_then(|res| {
                std::fs::read_dir(&res)
                    .ok()?
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .find(|path| path.extension().is_some_and(|ext| ext == "png"))
            });
        let Some(sample) = sample else {
            eprintln!("skipped: no sample PNG found next to {dir}");
            return;
        };

        let bytes = std::fs::read(&sample).expect("read sample image");
        // A current-thread runtime rather than `#[tokio::test]`: the crate only
        // asks tokio for `spawn_blocking`, so it stays out of the test macros.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build a runtime");
        let image = runtime.block_on(recognize_png(bytes)).expect("OCR the sample image");
        let text: String = image.lines.iter().map(|line| line.text.as_str()).collect();
        assert!(!text.trim().is_empty(), "no text recognized in {}", sample.display());
        assert!(image.width > 0 && image.height > 0, "the image size must survive OCR");
    }

    #[test]
    fn reports_the_directory_it_looked_in() {
        let missing = std::env::temp_dir().join("xphoneuse-ocr-does-not-exist");
        // Not `expect_err`: `OcrEngine` has no `Debug`, so the Ok arm cannot be
        // printed - and printing it would not help anyway.
        let err = match build_engine(&missing) {
            Ok(_) => panic!("a missing directory must not build an engine"),
            Err(err) => err,
        };
        assert!(err.contains(&missing.display().to_string()), "{err}");
        assert!(err.contains("XPHONEUSE_OCR_MODELS"), "{err}");
    }
}
