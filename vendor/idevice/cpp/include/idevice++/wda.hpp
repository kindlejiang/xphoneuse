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
#include <utility>
#include <vector>

namespace IdeviceFFI {

using WdaClientPtr =
    std::unique_ptr<WdaClientHandle, FnDeleter<WdaClientHandle, wda_client_free>>;

/// Minimal WebDriverAgent client over a single device's direct connections.
///
/// `Wda::create` consumes the `Provider` argument; subsequent calls reuse the
/// owned provider to open per-request sockets, matching the underlying Rust
/// `WdaClient` semantics.
class Wda {
  public:
    static Result<Wda, FfiError> create(Provider&& provider);

    // Builder-style configuration
    Result<void, FfiError>                         set_ports(uint16_t http, uint16_t mjpeg);
    Result<void, FfiError>                         set_timeout_ms(uint64_t ms);
    Result<std::pair<uint16_t, uint16_t>, FfiError> get_ports() const;
    Option<std::string>                            session_id() const;

    // Status / session
    Result<std::string, FfiError> status();
    Result<std::string, FfiError> wait_until_ready(uint64_t timeout_ms);
    Result<std::string, FfiError> start_session(Option<std::string> bundle_id);
    Result<void, FfiError>        delete_session(const std::string& session_id);

    // Element finding & state
    Result<std::string, FfiError>              find_element(const std::string&  using_,
                                                            const std::string&  value,
                                                            Option<std::string> session_id);
    Result<std::vector<std::string>, FfiError> find_elements(const std::string&  using_,
                                                             const std::string&  value,
                                                             Option<std::string> session_id);
    Result<std::string, FfiError>              element_attribute(const std::string&  element_id,
                                                                 const std::string&  name,
                                                                 Option<std::string> session_id);
    Result<std::string, FfiError>              element_text(const std::string&  element_id,
                                                            Option<std::string> session_id);
    Result<std::string, FfiError>              element_rect(const std::string&  element_id,
                                                            Option<std::string> session_id);
    Result<bool, FfiError> element_displayed(const std::string&  element_id,
                                             Option<std::string> session_id);
    Result<bool, FfiError> element_enabled(const std::string&  element_id,
                                           Option<std::string> session_id);
    Result<bool, FfiError> element_selected(const std::string&  element_id,
                                            Option<std::string> session_id);

    // Interaction
    Result<void, FfiError> click(const std::string& element_id, Option<std::string> session_id);
    Result<void, FfiError> send_keys(const std::string& text, Option<std::string> session_id);
    Result<void, FfiError> press_button(const std::string& name, Option<std::string> session_id);
    Result<void, FfiError> unlock(Option<std::string> session_id);
    Result<void, FfiError> swipe(int64_t             start_x,
                                 int64_t             start_y,
                                 int64_t             end_x,
                                 int64_t             end_y,
                                 double              duration,
                                 Option<std::string> session_id);
    Result<void, FfiError> tap(Option<double>      x,
                               Option<double>      y,
                               Option<std::string> element_id,
                               Option<std::string> session_id);
    Result<void, FfiError> double_tap(Option<double>      x,
                                      Option<double>      y,
                                      Option<std::string> element_id,
                                      Option<std::string> session_id);
    Result<void, FfiError> touch_and_hold(double              duration,
                                          Option<double>      x,
                                          Option<double>      y,
                                          Option<std::string> element_id,
                                          Option<std::string> session_id);
    Result<void, FfiError> scroll(Option<std::string> direction,
                                  Option<std::string> name,
                                  Option<std::string> predicate_string,
                                  Option<bool>        to_visible,
                                  Option<std::string> element_id,
                                  Option<std::string> session_id);

    // Output / app
    Result<std::string, FfiError>          source(Option<std::string> session_id);
    Result<std::vector<uint8_t>, FfiError> screenshot(Option<std::string> session_id);
    Result<std::string, FfiError>          window_size(Option<std::string> session_id);
    Result<std::string, FfiError>          viewport_rect(Option<std::string> session_id);
    Result<std::string, FfiError>          orientation(Option<std::string> session_id);
    Result<std::string, FfiError>          launch_app(const std::string&              bundle_id,
                                                      const std::vector<std::string>& arguments,
                                                      Option<std::string>             environment_json,
                                                      Option<std::string>             session_id);
    Result<std::string, FfiError>          activate_app(const std::string&  bundle_id,
                                                        Option<std::string> session_id);
    Result<bool, FfiError>                 terminate_app(const std::string&  bundle_id,
                                                         Option<std::string> session_id);
    Result<int64_t, FfiError>              query_app_state(const std::string&  bundle_id,
                                                           Option<std::string> session_id);
    Result<std::string, FfiError>          background_app(Option<double>      seconds,
                                                          Option<std::string> session_id);
    Result<bool, FfiError>                 is_locked(Option<std::string> session_id);

    // RAII / moves
    ~Wda() noexcept                  = default;
    Wda(Wda&&) noexcept              = default;
    Wda& operator=(Wda&&) noexcept   = default;
    Wda(const Wda&)                  = delete;
    Wda&             operator=(const Wda&) = delete;

    WdaClientHandle* raw() const noexcept { return handle_.get(); }
    static Wda       adopt(WdaClientHandle* h) noexcept { return Wda(h); }

  private:
    explicit Wda(WdaClientHandle* h) noexcept : handle_(h) {}
    WdaClientPtr handle_{};
};

} // namespace IdeviceFFI
