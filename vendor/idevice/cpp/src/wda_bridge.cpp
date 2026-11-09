// Jackson Coxson

#include <idevice++/wda_bridge.hpp>

namespace IdeviceFFI {

Result<WdaBridge, FfiError> WdaBridge::start(Provider&& provider) {
    WdaBridgeHandle* out = nullptr;
    FfiError         e(::wda_bridge_start(provider.raw(), &out));
    // Provider is consumed by the FFI regardless of outcome.
    provider.release();
    if (e) return Err(e);
    return Ok(WdaBridge::adopt(out));
}

Result<WdaBridge, FfiError> WdaBridge::start_with_ports(Provider&& provider,
                                                        uint16_t   device_http,
                                                        uint16_t   device_mjpeg) {
    WdaBridgeHandle* out = nullptr;
    FfiError e(::wda_bridge_start_with_ports(provider.raw(), device_http, device_mjpeg, &out));
    provider.release();
    if (e) return Err(e);
    return Ok(WdaBridge::adopt(out));
}

Result<WdaBridgeEndpoints, FfiError> WdaBridge::endpoints() const {
    WdaBridgeEndpointsC* raw = nullptr;
    FfiError             e(::wda_bridge_endpoints(handle_.get(), &raw));
    if (e) return Err(e);

    WdaBridgeEndpoints out;
    if (raw->udid) {
        out.udid = Some(std::string(raw->udid));
    }
    if (raw->wda_url) out.wda_url = raw->wda_url;
    if (raw->mjpeg_url) out.mjpeg_url = raw->mjpeg_url;
    out.local_http   = raw->local_http;
    out.local_mjpeg  = raw->local_mjpeg;
    out.device_http  = raw->device_http;
    out.device_mjpeg = raw->device_mjpeg;

    ::wda_bridge_endpoints_free(raw);
    return Ok(std::move(out));
}

} // namespace IdeviceFFI
