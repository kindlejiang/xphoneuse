// Jackson Coxson

#include <idevice++/wda.hpp>

namespace IdeviceFFI {

namespace {

inline const char* opt_cstr(const Option<std::string>& s) {
    return s.is_some() ? s.expect("checked").c_str() : nullptr;
}

inline std::string adopt_cstring(char* raw) {
    if (!raw) return {};
    std::string out(raw);
    ::idevice_string_free(raw);
    return out;
}

} // namespace

Result<Wda, FfiError> Wda::create(Provider&& provider) {
    WdaClientHandle* out = nullptr;
    FfiError         e(::wda_client_new(provider.raw(), &out));
    if (e) {
        // The provider was consumed by the FFI regardless of success.
        provider.release();
        return Err(e);
    }
    provider.release();
    return Ok(Wda::adopt(out));
}

Result<void, FfiError> Wda::set_ports(uint16_t http, uint16_t mjpeg) {
    FfiError e(::wda_client_set_ports(handle_.get(), http, mjpeg));
    if (e) return Err(e);
    return Ok();
}

Result<void, FfiError> Wda::set_timeout_ms(uint64_t ms) {
    FfiError e(::wda_client_set_timeout_ms(handle_.get(), ms));
    if (e) return Err(e);
    return Ok();
}

Result<std::pair<uint16_t, uint16_t>, FfiError> Wda::get_ports() const {
    uint16_t http  = 0;
    uint16_t mjpeg = 0;
    FfiError e(::wda_client_get_ports(handle_.get(), &http, &mjpeg));
    if (e) return Err(e);
    return Ok(std::make_pair(http, mjpeg));
}

Option<std::string> Wda::session_id() const {
    char*    raw = nullptr;
    FfiError e(::wda_client_session_id(handle_.get(), &raw));
    if (e || !raw) return None;
    return Some(adopt_cstring(raw));
}

Result<std::string, FfiError> Wda::status() {
    char*    raw = nullptr;
    FfiError e(::wda_client_status(handle_.get(), &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<std::string, FfiError> Wda::wait_until_ready(uint64_t timeout_ms) {
    char*    raw = nullptr;
    FfiError e(::wda_client_wait_until_ready(handle_.get(), timeout_ms, &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<std::string, FfiError> Wda::start_session(Option<std::string> bundle_id) {
    char*    raw = nullptr;
    FfiError e(::wda_client_start_session(handle_.get(), opt_cstr(bundle_id), &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<void, FfiError> Wda::delete_session(const std::string& session_id) {
    FfiError e(::wda_client_delete_session(handle_.get(), session_id.c_str()));
    if (e) return Err(e);
    return Ok();
}

Result<std::string, FfiError> Wda::find_element(const std::string&  using_,
                                                const std::string&  value,
                                                Option<std::string> session_id) {
    char*    raw = nullptr;
    FfiError e(::wda_client_find_element(
        handle_.get(), using_.c_str(), value.c_str(), opt_cstr(session_id), &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<std::vector<std::string>, FfiError> Wda::find_elements(const std::string&  using_,
                                                              const std::string&  value,
                                                              Option<std::string> session_id) {
    char**   arr   = nullptr;
    size_t   count = 0;
    FfiError e(::wda_client_find_elements(handle_.get(),
                                          using_.c_str(),
                                          value.c_str(),
                                          opt_cstr(session_id),
                                          &arr,
                                          &count));
    if (e) return Err(e);

    std::vector<std::string> out;
    out.reserve(count);
    for (size_t i = 0; i < count; ++i) {
        out.emplace_back(arr[i] ? arr[i] : "");
    }
    ::wda_client_string_array_free(arr, count);
    return Ok(std::move(out));
}

Result<std::string, FfiError> Wda::element_attribute(const std::string&  element_id,
                                                     const std::string&  name,
                                                     Option<std::string> session_id) {
    char*    raw = nullptr;
    FfiError e(::wda_client_element_attribute(
        handle_.get(), element_id.c_str(), name.c_str(), opt_cstr(session_id), &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<std::string, FfiError> Wda::element_text(const std::string&  element_id,
                                                Option<std::string> session_id) {
    char*    raw = nullptr;
    FfiError e(::wda_client_element_text(
        handle_.get(), element_id.c_str(), opt_cstr(session_id), &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<std::string, FfiError> Wda::element_rect(const std::string&  element_id,
                                                Option<std::string> session_id) {
    char*    raw = nullptr;
    FfiError e(::wda_client_element_rect(
        handle_.get(), element_id.c_str(), opt_cstr(session_id), &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<bool, FfiError> Wda::element_displayed(const std::string&  element_id,
                                              Option<std::string> session_id) {
    bool     out = false;
    FfiError e(::wda_client_element_displayed(
        handle_.get(), element_id.c_str(), opt_cstr(session_id), &out));
    if (e) return Err(e);
    return Ok(out);
}

Result<bool, FfiError> Wda::element_enabled(const std::string&  element_id,
                                            Option<std::string> session_id) {
    bool     out = false;
    FfiError e(::wda_client_element_enabled(
        handle_.get(), element_id.c_str(), opt_cstr(session_id), &out));
    if (e) return Err(e);
    return Ok(out);
}

Result<bool, FfiError> Wda::element_selected(const std::string&  element_id,
                                             Option<std::string> session_id) {
    bool     out = false;
    FfiError e(::wda_client_element_selected(
        handle_.get(), element_id.c_str(), opt_cstr(session_id), &out));
    if (e) return Err(e);
    return Ok(out);
}

Result<void, FfiError> Wda::click(const std::string& element_id, Option<std::string> session_id) {
    FfiError e(::wda_client_click(handle_.get(), element_id.c_str(), opt_cstr(session_id)));
    if (e) return Err(e);
    return Ok();
}

Result<void, FfiError> Wda::send_keys(const std::string& text, Option<std::string> session_id) {
    FfiError e(::wda_client_send_keys(handle_.get(), text.c_str(), opt_cstr(session_id)));
    if (e) return Err(e);
    return Ok();
}

Result<void, FfiError> Wda::press_button(const std::string&  name,
                                         Option<std::string> session_id) {
    FfiError e(::wda_client_press_button(handle_.get(), name.c_str(), opt_cstr(session_id)));
    if (e) return Err(e);
    return Ok();
}

Result<void, FfiError> Wda::unlock(Option<std::string> session_id) {
    FfiError e(::wda_client_unlock(handle_.get(), opt_cstr(session_id)));
    if (e) return Err(e);
    return Ok();
}

Result<void, FfiError> Wda::swipe(int64_t             start_x,
                                  int64_t             start_y,
                                  int64_t             end_x,
                                  int64_t             end_y,
                                  double              duration,
                                  Option<std::string> session_id) {
    FfiError e(::wda_client_swipe(
        handle_.get(), start_x, start_y, end_x, end_y, duration, opt_cstr(session_id)));
    if (e) return Err(e);
    return Ok();
}

Result<void, FfiError> Wda::tap(Option<double>      x,
                                Option<double>      y,
                                Option<std::string> element_id,
                                Option<std::string> session_id) {
    bool     has_x = x.is_some();
    bool     has_y = y.is_some();
    double   xv    = has_x ? x.expect("checked") : 0.0;
    double   yv    = has_y ? y.expect("checked") : 0.0;
    FfiError e(::wda_client_tap(
        handle_.get(), has_x, xv, has_y, yv, opt_cstr(element_id), opt_cstr(session_id)));
    if (e) return Err(e);
    return Ok();
}

Result<void, FfiError> Wda::double_tap(Option<double>      x,
                                       Option<double>      y,
                                       Option<std::string> element_id,
                                       Option<std::string> session_id) {
    bool     has_x = x.is_some();
    bool     has_y = y.is_some();
    double   xv    = has_x ? x.expect("checked") : 0.0;
    double   yv    = has_y ? y.expect("checked") : 0.0;
    FfiError e(::wda_client_double_tap(
        handle_.get(), has_x, xv, has_y, yv, opt_cstr(element_id), opt_cstr(session_id)));
    if (e) return Err(e);
    return Ok();
}

Result<void, FfiError> Wda::touch_and_hold(double              duration,
                                           Option<double>      x,
                                           Option<double>      y,
                                           Option<std::string> element_id,
                                           Option<std::string> session_id) {
    bool     has_x = x.is_some();
    bool     has_y = y.is_some();
    double   xv    = has_x ? x.expect("checked") : 0.0;
    double   yv    = has_y ? y.expect("checked") : 0.0;
    FfiError e(::wda_client_touch_and_hold(handle_.get(),
                                           duration,
                                           has_x,
                                           xv,
                                           has_y,
                                           yv,
                                           opt_cstr(element_id),
                                           opt_cstr(session_id)));
    if (e) return Err(e);
    return Ok();
}

Result<void, FfiError> Wda::scroll(Option<std::string> direction,
                                   Option<std::string> name,
                                   Option<std::string> predicate_string,
                                   Option<bool>        to_visible,
                                   Option<std::string> element_id,
                                   Option<std::string> session_id) {
    bool     has_to_visible = to_visible.is_some();
    bool     to_visible_v   = has_to_visible ? to_visible.expect("checked") : false;
    FfiError e(::wda_client_scroll(handle_.get(),
                                   opt_cstr(direction),
                                   opt_cstr(name),
                                   opt_cstr(predicate_string),
                                   has_to_visible,
                                   to_visible_v,
                                   opt_cstr(element_id),
                                   opt_cstr(session_id)));
    if (e) return Err(e);
    return Ok();
}

Result<std::string, FfiError> Wda::source(Option<std::string> session_id) {
    char*    raw = nullptr;
    FfiError e(::wda_client_source(handle_.get(), opt_cstr(session_id), &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<std::vector<uint8_t>, FfiError> Wda::screenshot(Option<std::string> session_id) {
    uint8_t* bytes = nullptr;
    size_t   len   = 0;
    FfiError e(::wda_client_screenshot(handle_.get(), opt_cstr(session_id), &bytes, &len));
    if (e) return Err(e);

    std::vector<uint8_t> out;
    if (bytes && len > 0) {
        out.assign(bytes, bytes + len);
    }
    ::idevice_data_free(bytes, len);
    return Ok(std::move(out));
}

Result<std::string, FfiError> Wda::window_size(Option<std::string> session_id) {
    char*    raw = nullptr;
    FfiError e(::wda_client_window_size(handle_.get(), opt_cstr(session_id), &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<std::string, FfiError> Wda::viewport_rect(Option<std::string> session_id) {
    char*    raw = nullptr;
    FfiError e(::wda_client_viewport_rect(handle_.get(), opt_cstr(session_id), &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<std::string, FfiError> Wda::orientation(Option<std::string> session_id) {
    char*    raw = nullptr;
    FfiError e(::wda_client_orientation(handle_.get(), opt_cstr(session_id), &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<std::string, FfiError> Wda::launch_app(const std::string&              bundle_id,
                                              const std::vector<std::string>& arguments,
                                              Option<std::string>             environment_json,
                                              Option<std::string>             session_id) {
    std::vector<const char*> c_args;
    c_args.reserve(arguments.size());
    for (const auto& a : arguments) c_args.push_back(a.c_str());

    char*    raw = nullptr;
    FfiError e(::wda_client_launch_app(handle_.get(),
                                       bundle_id.c_str(),
                                       c_args.empty() ? nullptr : c_args.data(),
                                       c_args.size(),
                                       opt_cstr(environment_json),
                                       opt_cstr(session_id),
                                       &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<std::string, FfiError> Wda::activate_app(const std::string&  bundle_id,
                                                Option<std::string> session_id) {
    char*    raw = nullptr;
    FfiError e(::wda_client_activate_app(
        handle_.get(), bundle_id.c_str(), opt_cstr(session_id), &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<bool, FfiError> Wda::terminate_app(const std::string&  bundle_id,
                                          Option<std::string> session_id) {
    bool     out = false;
    FfiError e(::wda_client_terminate_app(
        handle_.get(), bundle_id.c_str(), opt_cstr(session_id), &out));
    if (e) return Err(e);
    return Ok(out);
}

Result<int64_t, FfiError> Wda::query_app_state(const std::string&  bundle_id,
                                               Option<std::string> session_id) {
    int64_t  out = 0;
    FfiError e(::wda_client_query_app_state(
        handle_.get(), bundle_id.c_str(), opt_cstr(session_id), &out));
    if (e) return Err(e);
    return Ok(out);
}

Result<std::string, FfiError> Wda::background_app(Option<double>      seconds,
                                                  Option<std::string> session_id) {
    bool     has_seconds = seconds.is_some();
    double   seconds_v   = has_seconds ? seconds.expect("checked") : 0.0;
    char*    raw         = nullptr;
    FfiError e(::wda_client_background_app(
        handle_.get(), has_seconds, seconds_v, opt_cstr(session_id), &raw));
    if (e) return Err(e);
    return Ok(adopt_cstring(raw));
}

Result<bool, FfiError> Wda::is_locked(Option<std::string> session_id) {
    bool     out = false;
    FfiError e(::wda_client_is_locked(handle_.get(), opt_cstr(session_id), &out));
    if (e) return Err(e);
    return Ok(out);
}

} // namespace IdeviceFFI
