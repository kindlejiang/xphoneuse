//! The Cryptex developer image (iOS 17.4+, and the only variant newer
//! hardware accepts).
//!
//! The personalized DDI the app mounts at `/System/Developer` is signed for a
//! specific `ApChipID`/`ApBoardID` pair, and the mirror only carries the
//! hardware that existed when Apple built that Xcode. A device released after
//! it - the iPhone 18 series is the first - has no identity in that manifest,
//! and the mount fails with `BadBuildManifest`. For those devices Xcode
//! installs a **Cryptex1** image instead: the same DDI payloads, but
//! announced to `cryptexd` over RSD and personalized with a Cryptex1 ticket
//! that names no device at all, so Apple's server decides eligibility.
//!
//! The protocol mirrors pymobiledevice3's `CryptexdService`:
//!
//! * `read-personalization-id` → the device's `img4_chip_*` dictionary (the
//!   TSS ticket is built from it);
//! * `get-nonce` for the identity's `Cryptex1,NonceDomain` handle → the nonce
//!   blob, which carries the nonce at offset 2;
//! * a Cryptex1 TSS request (`tss::add_cryptex1_tags`) → `Cryptex1,Ticket`;
//! * one `install` request on one connection, followed by five file transfers
//!   (image, trustcache, im4m, info, volumehash) - the daemon replies only
//!   after consuming all of them;
//! * `copy-installed` to confirm `com.apple.MobileAsset.DDI` is there.
//!
//! The assets live next to the personalized ones in the same GitHub mirror
//! (`PersonalizedImages/Xcode_iOS_DDI_Cryptex/`), so they share the download
//! bases and the cache directory.

use std::path::{Path, PathBuf};
use std::time::Duration;

use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::rsd::RsdHandshake;
use idevice::tcp::handle::AdapterHandle;
use idevice::tss::{select_cryptex_build_identity, TSSRequest};
use idevice::xpc::{Dictionary as XpcDictionary, RemoteXpcClient, XPCObject};
use idevice::{IdeviceService, ReadWrite};

use crate::developer_image::{report, Downloader, ImageProgress};
use crate::device::UsbmuxdProviderBox;

/// The cryptex daemon's RSD service.
const SVC_CRYPTEXD: &str = "com.apple.security.cryptexd.remote";
/// The Cryptex1 bundle inside the mirror repository.
const CRYPTEX_DIR: &str = "PersonalizedImages/Xcode_iOS_DDI_Cryptex";
/// The identity Apple recognizes as the developer disk image.
const DDI_IDENTIFIER: &str = "com.apple.MobileAsset.DDI";
/// How long `install` may take to answer once the payloads are sent. The
/// daemon replies only after consuming every transfer, so silence means it
/// refused the request without answering.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Ordinary `routine` calls answer immediately.
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);
/// `image-type-index` for the generic DMG (GDMG) asset type.
const IMAGE_TYPE_GDMG: i64 = 10;

/// The manifest tag → local file name for the four payloads.
const PAYLOADS: [(&str, &str); 4] = [
    ("Cryptex1,GenericDmg", "image.dmg"),
    ("Cryptex1,GenericTrustCache", "trustcache"),
    ("Cryptex1,CryptexInfoPlist", "cryptex_info"),
    ("Cryptex1,GenericVolume", "root_hash"),
];

/// A Cryptex1 developer image, downloaded and ready to install.
pub struct CryptexAssets {
    /// The build id the cache directory is named after.
    pub build: String,
    /// The build manifest's Cryptex identity (the one whose `Info.Variant`
    /// names the developer disk image).
    identity: plist::Dictionary,
    image: Vec<u8>,
    trustcache: Vec<u8>,
    info: Vec<u8>,
    volumehash: Vec<u8>,
    pub downloaded: bool,
}

/// The cryptex bundle for this cache directory, downloading it when it is not
/// there yet.
pub async fn fetch_assets(cache_dir: &Path) -> Result<CryptexAssets, String> {
    if let Some(assets) = read_cached(cache_dir) {
        return Ok(assets);
    }
    std::fs::create_dir_all(cache_dir)
        .map_err(|e| format!("create {}: {e}", cache_dir.display()))?;

    let mut downloader = Downloader::new();
    let manifest_tmp = cache_dir.join("cryptex-BuildManifest.plist.part");
    downloader
        .to_file(&format!("{CRYPTEX_DIR}/BuildManifest.plist"), &manifest_tmp)
        .await?;
    let manifest_bytes =
        std::fs::read(&manifest_tmp).map_err(|e| format!("read cryptex manifest: {e}"))?;
    let manifest: plist::Value = plist::from_bytes(&manifest_bytes)
        .map_err(|e| format!("cryptex BuildManifest.plist: {e}"))?;
    let manifest = manifest
        .as_dictionary()
        .ok_or_else(|| "cryptex BuildManifest.plist is not a dictionary".to_string())?;
    let build = manifest
        .get("ProductBuildVersion")
        .and_then(|v| v.as_string())
        .ok_or_else(|| "cryptex BuildManifest.plist has no ProductBuildVersion".to_string())?
        .to_string();
    let identity = select_cryptex_build_identity(manifest)
        .map_err(|e| format!("cryptex BuildManifest.plist: {e:?}"))?
        .clone();

    let dir = cache_dir.join(format!("cryptex-{build}"));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    std::fs::rename(&manifest_tmp, dir.join("BuildManifest.plist"))
        .map_err(|e| format!("store cryptex manifest: {e}"))?;

    for (tag, local) in PAYLOADS {
        let remote = payload_path(&identity, tag)?;
        downloader
            .to_file(&format!("{CRYPTEX_DIR}/{remote}"), &dir.join(local))
            .await?;
    }

    let read = |name: &str| -> Result<Vec<u8>, String> {
        std::fs::read(dir.join(name)).map_err(|e| format!("read cryptex {name}: {e}"))
    };
    Ok(CryptexAssets {
        build,
        identity,
        image: read("image.dmg")?,
        trustcache: read("trustcache")?,
        info: read("cryptex_info")?,
        volumehash: read("root_hash")?,
        downloaded: true,
    })
}

/// The `Restore`-relative path of a payload, from the identity's manifest.
fn payload_path(identity: &plist::Dictionary, tag: &str) -> Result<String, String> {
    identity
        .get("Manifest")
        .and_then(|m| m.as_dictionary())
        .and_then(|m| m.get(tag))
        .and_then(|entry| entry.as_dictionary())
        .and_then(|entry| entry.get("Info"))
        .and_then(|info| info.as_dictionary())
        .and_then(|info| info.get("Path"))
        .and_then(|p| p.as_string())
        .map(str::to_string)
        .ok_or_else(|| format!("cryptex build manifest has no Manifest[{tag}].Info.Path"))
}

/// A complete cryptex bundle already on disk, if any.
fn read_cached(cache_dir: &Path) -> Option<CryptexAssets> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(cache_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("cryptex-"))
        })
        .collect();
    dirs.sort();

    for dir in dirs.into_iter().rev() {
        let complete = ["BuildManifest.plist", "image.dmg", "trustcache", "cryptex_info", "root_hash"]
            .iter()
            .all(|name| dir.join(name).is_file());
        if !complete {
            continue;
        }
        let manifest_bytes = std::fs::read(dir.join("BuildManifest.plist")).ok()?;
        let manifest: plist::Value = plist::from_bytes(&manifest_bytes).ok()?;
        let identity = select_cryptex_build_identity(manifest.as_dictionary()?).ok()?.clone();
        let build = identity
            .get("Info")
            .and_then(|i| i.as_dictionary())
            .and_then(|i| i.get("BuildNumber"))
            .and_then(|b| b.as_string())
            .unwrap_or_default()
            .to_string();
        let read = |name: &str| std::fs::read(dir.join(name)).ok();
        return Some(CryptexAssets {
            build: build.clone(),
            identity,
            image: read("image.dmg")?,
            trustcache: read("trustcache")?,
            info: read("cryptex_info")?,
            volumehash: read("root_hash")?,
            downloaded: false,
        });
    }
    None
}

/// Install the cryptex bundle on the device and confirm `cryptexd` reports it.
pub async fn install(provider: &UsbmuxdProviderBox, assets: &CryptexAssets) -> Result<(), String> {
    let (mut adapter, handshake) = connect_rsd(provider).await?;

    let chip_instance = {
        let mut client = Cryptexd::connect(&mut adapter, &handshake).await?;
        client
            .invoke("read-personalization-id", XpcDictionary::new())
            .await?
    };
    let nonce_domain = manifest_u64(&assets.identity, "Cryptex1,NonceDomain")?;
    let nonce_blob = {
        let mut client = Cryptexd::connect(&mut adapter, &handshake).await?;
        let mut argv = XpcDictionary::new();
        argv.insert(
            "nonce-domain-handle".to_string(),
            XPCObject::UInt64(nonce_domain),
        );
        let reply = client.invoke("get-nonce", argv).await?;
        match reply.get("nonce") {
            Some(plist::Value::Data(blob)) => blob.clone(),
            _ => return Err("cryptexd get-nonce returned no nonce".to_string()),
        }
    };
    let nonce = nonce_from_blob(&nonce_blob)?;
    eprintln!(
        "[ddi-cryptex] chip instance ok, nonce domain {nonce_domain}, nonce {}B",
        nonce.len()
    );

    let mut request = TSSRequest::new();
    request
        .add_cryptex1_tags(&assets.identity, &chip_instance, &nonce)
        .map_err(|e| format!("Cryptex1 TSS request: {e:?}"))?;
    let response = request.send().await.map_err(|e| format!("Cryptex1 TSS: {e:?}"))?;
    let ticket = tss_ticket(&response)?;

    let mut properties = XpcDictionary::new();
    properties.insert(
        "Cryptex1,UseProductClass".to_string(),
        XPCObject::Bool(manifest_bool(&assets.identity, "Cryptex1,UseProductClass")?),
    );
    properties.insert("MountedCryptex".to_string(), XPCObject::Bool(false));
    properties.insert(
        "Cryptex1,SubType".to_string(),
        XPCObject::UInt64(manifest_u64(&assets.identity, "Cryptex1,SubType")?),
    );
    properties.insert(
        "Cryptex1,NonceDomain".to_string(),
        XPCObject::UInt64(nonce_domain),
    );
    properties.insert(
        "Cryptex1,Version".to_string(),
        XPCObject::String(manifest_string(&assets.identity, "Cryptex1,Version")?),
    );
    properties.insert(
        "Cryptex1,PreauthorizationVersion".to_string(),
        XPCObject::String(manifest_string(
            &assets.identity,
            "Cryptex1,PreauthorizationVersion",
        )?),
    );

    let payloads: [(&str, &[u8]); 5] = [
        ("image", &assets.image),
        ("trustcache", &assets.trustcache),
        ("im4m", &ticket),
        ("info", &assets.info),
        ("volumehash", &assets.volumehash),
    ];

    let mut argv = XpcDictionary::new();
    argv.insert("auth".to_string(), XPCObject::UInt64(0));
    argv.insert("client-version".to_string(), XPCObject::UInt64(3));
    argv.insert(
        "cryptex1-properties".to_string(),
        XPCObject::Dictionary(properties),
    );
    argv.insert(
        "image-type-index".to_string(),
        XPCObject::Int64(IMAGE_TYPE_GDMG),
    );
    argv.insert("nonce-persistence".to_string(), XPCObject::UInt64(1));
    argv.insert("persistence".to_string(), XPCObject::UInt64(2));
    for (id, (key, payload)) in payloads.iter().enumerate() {
        let id = id as u64 + 1;
        let mut size = XpcDictionary::new();
        size.insert("s".to_string(), XPCObject::UInt64(payload.len() as u64));
        argv.insert(
            (*key).to_string(),
            XPCObject::FileTransfer {
                msg_id: id,
                data: Box::new(XPCObject::Dictionary(size)),
            },
        );
    }

    report(ImageProgress::Mounting { sent: 0, total: 0 });
    let mut client = Cryptexd::connect(&mut adapter, &handshake).await?;
    let mut message = XpcDictionary::new();
    message.insert("routine".to_string(), XPCObject::String("install".into()));
    message.insert("argv".to_string(), XPCObject::Dictionary(argv));
    client
        .xpc
        .send_object(XPCObject::Dictionary(message), true)
        .await
        .map_err(|e| format!("cryptexd install: send: {e:?}"))?;
    for (id, (_, payload)) in payloads.iter().enumerate() {
        client
            .xpc
            .send_file_transfer(id as u64 + 1, payload)
            .await
            .map_err(|e| format!("cryptexd install: transfer {}: {e:?}", id + 1))?;
    }
    let reply = tokio::time::timeout(INSTALL_TIMEOUT, client.xpc.recv())
        .await
        .map_err(|_| {
            format!(
                "cryptexd did not answer the install within {}s - it most likely \
                 rejected the request without replying",
                INSTALL_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("cryptexd install: receive: {e:?}"))?;
    unwrap_reply("install", reply)?;

    // The daemon acknowledging the request is not the same as the DDI being
    // registered; ask what is installed now.
    let mut verifier = Cryptexd::connect(&mut adapter, &handshake).await?;
    let installed = verifier.invoke("copy-installed", XpcDictionary::new()).await?;
    if !installed_has_ddi(&installed) {
        return Err(format!(
            "cryptexd accepted the install but does not list {DDI_IDENTIFIER}"
        ));
    }
    eprintln!("[ddi-cryptex] installed {} (build {})", DDI_IDENTIFIER, assets.build);
    Ok(())
}

/// A Cryptex1 or (fallback) AP img4 ticket from a TSS response.
fn tss_ticket(response: &plist::Value) -> Result<Vec<u8>, String> {
    let dict = response
        .as_dictionary()
        .ok_or_else(|| "Cryptex1 TSS response is not a dictionary".to_string())?;
    for key in ["Cryptex1,Ticket", "ApImg4Ticket"] {
        if let Some(plist::Value::Data(ticket)) = dict.get(key) {
            return Ok(ticket.clone());
        }
    }
    Err("Cryptex1 TSS response carries no ticket".to_string())
}

/// `copy-installed`'s reply lists the cryptexes the device has; the DDI is
/// identified by `com.apple.MobileAsset.DDI`.
fn installed_has_ddi(argv: &plist::Dictionary) -> bool {
    argv.get("remote-cryptex-array")
        .and_then(|list| list.as_array())
        .is_some_and(|list| {
            list.iter().any(|entry| {
                entry
                    .as_dictionary()
                    .and_then(|entry| entry.get("remote-cryptex-identifier"))
                    .and_then(|id| id.as_string())
                    == Some(DDI_IDENTIFIER)
            })
        })
}

/// The nonce is wrapped in a small structure: the payload starts at offset 2
/// and its length is the little-endian u32 in the last four bytes.
fn nonce_from_blob(blob: &[u8]) -> Result<Vec<u8>, String> {
    if blob.len() < 6 {
        return Err(format!("cryptex nonce blob is {} bytes", blob.len()));
    }
    let len = u32::from_le_bytes(blob[blob.len() - 4..].try_into().unwrap()) as usize;
    if blob.len() < 2 + len {
        return Err(format!(
            "cryptex nonce blob says {len} bytes but carries {}",
            blob.len()
        ));
    }
    Ok(blob[2..2 + len].to_vec())
}

/// A cryptexd RemoteXPC connection. One per routine call, as the daemon
/// expects; the `install` path keeps its connection for the file transfers.
struct Cryptexd {
    xpc: RemoteXpcClient<Box<dyn ReadWrite>>,
}

impl Cryptexd {
    async fn connect(
        adapter: &mut AdapterHandle,
        handshake: &RsdHandshake,
    ) -> Result<Self, String> {
        let stream = crate::tunnel::open_rsd_stream(adapter, handshake, SVC_CRYPTEXD).await?;
        let mut xpc = RemoteXpcClient::new(stream)
            .await
            .map_err(|e| format!("cryptexd handshake: {e:?}"))?;
        xpc.do_handshake()
            .await
            .map_err(|e| format!("cryptexd do_handshake: {e:?}"))?;
        Ok(Self { xpc })
    }

    /// `{"routine": name, "argv": argv}` → the reply's `argv`.
    async fn invoke(
        &mut self,
        routine: &str,
        argv: XpcDictionary,
    ) -> Result<plist::Dictionary, String> {
        let mut message = XpcDictionary::new();
        message.insert("routine".to_string(), XPCObject::String(routine.into()));
        message.insert("argv".to_string(), XPCObject::Dictionary(argv));
        self.xpc
            .send_object(XPCObject::Dictionary(message), true)
            .await
            .map_err(|e| format!("cryptexd {routine}: send: {e:?}"))?;
        let reply = tokio::time::timeout(REPLY_TIMEOUT, self.xpc.recv())
            .await
            .map_err(|_| {
                format!(
                    "cryptexd {routine}: no reply within {}s",
                    REPLY_TIMEOUT.as_secs()
                )
            })?
            .map_err(|e| format!("cryptexd {routine}: receive: {e:?}"))?;
        unwrap_reply(routine, reply)
    }
}

/// A successful reply is `{"error": 0, "argv": ...}` (a few routines omit
/// `error`); failures carry a `cferr` dictionary or a nonzero Darwin `error`.
fn unwrap_reply(routine: &str, reply: plist::Value) -> Result<plist::Dictionary, String> {
    let dict = match reply {
        plist::Value::Dictionary(d) => d,
        other => return Err(format!("cryptexd {routine}: unexpected reply {other:?}")),
    };
    if let Some(error) = dict.get("cferr") {
        return Err(format!("cryptexd {routine}: {error:?}"));
    }
    if let Some(error) = dict.get("error") {
        let code = match error {
            plist::Value::Integer(n) => n.as_unsigned().unwrap_or(u64::MAX),
            _ => u64::MAX,
        };
        if code != 0 {
            return Err(format!("cryptexd {routine}: error {code}"));
        }
    }
    Ok(dict
        .get("argv")
        .and_then(|v| v.as_dictionary().cloned())
        .unwrap_or_default())
}

/// Build the RSD tunnel the cryptexd services are reached through. This needs
/// no developer image of its own - `remoted` is always there.
async fn connect_rsd(
    provider: &UsbmuxdProviderBox,
) -> Result<(AdapterHandle, RsdHandshake), String> {
    let proxy = CoreDeviceProxy::connect(provider.as_ref())
        .await
        .map_err(|e| format!("CoreDeviceProxy: {e:?}"))?;
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let mut adapter = proxy
        .create_software_tunnel()
        .map_err(|e| format!("software tunnel: {e:?}"))?
        .to_async_handle();
    let stream = adapter
        .connect(rsd_port)
        .await
        .map_err(|e| format!("RSD connect: {e:?}"))?;
    let handshake = RsdHandshake::new(stream)
        .await
        .map_err(|e| format!("RSD handshake: {e:?}"))?;
    Ok((adapter, handshake))
}

fn manifest_u64(identity: &plist::Dictionary, key: &str) -> Result<u64, String> {
    match identity.get(key) {
        Some(plist::Value::Integer(n)) => n
            .as_unsigned()
            .ok_or_else(|| format!("cryptex identity {key} is not unsigned")),
        Some(plist::Value::String(s)) => {
            let trimmed = s.trim_start_matches("0x");
            let radix = if trimmed.len() == s.len() { 10 } else { 16 };
            u64::from_str_radix(trimmed, radix).map_err(|_| format!("{key} is not a number"))
        }
        _ => Err(format!("cryptex identity has no {key}")),
    }
}

fn manifest_string(identity: &plist::Dictionary, key: &str) -> Result<String, String> {
    identity
        .get(key)
        .and_then(|v| v.as_string())
        .map(str::to_string)
        .ok_or_else(|| format!("cryptex identity has no {key}"))
}

fn manifest_bool(identity: &plist::Dictionary, key: &str) -> Result<bool, String> {
    identity
        .get(key)
        .and_then(|v| v.as_boolean())
        .ok_or_else(|| format!("cryptex identity has no {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nonce_is_read_from_the_length_prefixed_blob() {
        let mut blob = vec![0u8, 1];
        blob.extend_from_slice(&[7u8; 48]);
        blob.extend_from_slice(&48u32.to_le_bytes());
        assert_eq!(nonce_from_blob(&blob).unwrap(), vec![7u8; 48]);
    }

    #[test]
    fn a_truncated_nonce_blob_is_an_error() {
        let mut blob = vec![0u8, 1];
        blob.extend_from_slice(&[7u8; 10]);
        blob.extend_from_slice(&48u32.to_le_bytes());
        assert!(nonce_from_blob(&blob).is_err());
    }
}
