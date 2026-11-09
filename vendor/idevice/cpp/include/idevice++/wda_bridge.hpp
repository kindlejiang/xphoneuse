// Jackson Coxson

#pragma once
#include <cstdint>
#include <idevice++/bindings.hpp>
#include <idevice++/ffi.hpp>
#include <idevice++/option.hpp>
#include <idevice++/provider.hpp>
#include <idevice++/result.hpp>
#include <memory>
#include <string>

namespace IdeviceFFI {

using WdaBridgePtr =
    std::unique_ptr<WdaBridgeHandle, FnDeleter<WdaBridgeHandle, wda_bridge_free>>;

/// Localhost endpoints exposed by a running WDA bridge.
struct WdaBridgeEndpoints {
    Option<std::string> udid;
    std::string         wda_url;
    std::string         mjpeg_url;
    uint16_t            local_http   = 0;
    uint16_t            local_mjpeg  = 0;
    uint16_t            device_http  = 0;
    uint16_t            device_mjpeg = 0;
};

/// Dynamic localhost bridge for a single device's WDA endpoints.
///
/// Both factories consume the `Provider` argument; dropping the bridge aborts
/// the underlying forwarder tasks.
class WdaBridge {
  public:
    static Result<WdaBridge, FfiError> start(Provider&& provider);
    static Result<WdaBridge, FfiError> start_with_ports(Provider&& provider,
                                                        uint16_t   device_http,
                                                        uint16_t   device_mjpeg);

    Result<WdaBridgeEndpoints, FfiError> endpoints() const;

    ~WdaBridge() noexcept                        = default;
    WdaBridge(WdaBridge&&) noexcept              = default;
    WdaBridge& operator=(WdaBridge&&) noexcept   = default;
    WdaBridge(const WdaBridge&)                  = delete;
    WdaBridge&       operator=(const WdaBridge&) = delete;

    WdaBridgeHandle* raw() const noexcept { return handle_.get(); }
    static WdaBridge adopt(WdaBridgeHandle* h) noexcept { return WdaBridge(h); }

  private:
    explicit WdaBridge(WdaBridgeHandle* h) noexcept : handle_(h) {}
    WdaBridgePtr handle_{};
};

} // namespace IdeviceFFI
