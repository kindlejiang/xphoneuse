// Jackson Coxson

#include <cstdlib>
#include <fstream>
#include <iostream>
#include <string>

#include <idevice++/ffi.hpp>
#include <idevice++/provider.hpp>
#include <idevice++/usbmuxd.hpp>
#include <idevice++/wda.hpp>
#include <idevice++/wda_bridge.hpp>

using namespace IdeviceFFI;

static void print_usage(const char* prog) {
    std::cerr << "Usage:\n"
              << "  " << prog << " status\n"
              << "  " << prog << " screenshot <output.png> [bundle_id]\n"
              << "  " << prog << " bridge\n";
}

static Provider build_provider(const std::string& label) {
    auto mux     = UsbmuxdConnection::default_new(0).expect("failed to connect to usbmuxd");
    auto devices = mux.get_devices().expect("failed to list devices");
    if (devices.empty()) {
        std::cerr << "no devices connected\n";
        std::exit(1);
    }
    auto& dev = devices[0];

    auto udid = dev.get_udid();
    if (udid.is_none()) {
        std::cerr << "device has no UDID\n";
        std::exit(1);
    }
    auto mux_id = dev.get_id();
    if (mux_id.is_none()) {
        std::cerr << "device has no mux id\n";
        std::exit(1);
    }

    auto              addr = UsbmuxdAddr::default_new();
    const uint32_t    tag  = 0;
    return Provider::usbmuxd_new(std::move(addr), tag, udid.unwrap(), mux_id.unwrap(), label)
        .expect("failed to create provider");
}

int main(int argc, char** argv) {
    idevice_init_logger(Debug, Disabled, NULL);

    // Usage:
    //   wda status
    //   wda screenshot <output.png> [bundle_id]
    //   wda bridge
    if (argc < 2) {
        print_usage(argv[0]);
        return 2;
    }

    const std::string command = argv[1];

    if (command == "status") {
        if (argc != 2) {
            print_usage(argv[0]);
            return 2;
        }

        auto wda = Wda::create(build_provider("wda-client")).expect("failed to create Wda client");

        auto status = wda.status().expect("failed to fetch /status from WDA");
        std::cout << status << "\n";
        return 0;
    }

    if (command == "screenshot") {
        if (argc < 3 || argc > 4) {
            print_usage(argv[0]);
            return 2;
        }

        const std::string  out_path = argv[2];
        Option<std::string> bundle_id;
        if (argc == 4) {
            bundle_id = Some(std::string(argv[3]));
        }

        auto wda = Wda::create(build_provider("wda-client")).expect("failed to create Wda client");

        auto session_id = wda.start_session(bundle_id).expect("failed to start WDA session");
        std::cout << "Session: " << session_id << "\n";

        auto buf = wda.screenshot(None).expect("failed to capture screenshot");

        wda.delete_session(session_id).expect("failed to delete WDA session");

        std::ofstream out(out_path, std::ios::binary);
        if (!out.is_open()) {
            std::cerr << "failed to open output file: " << out_path << "\n";
            return 1;
        }
        out.write(reinterpret_cast<const char*>(buf.data()),
                  static_cast<std::streamsize>(buf.size()));
        out.close();

        std::cout << "Screenshot saved to " << out_path << " (" << buf.size() << " bytes)\n";
        return 0;
    }

    if (command == "bridge") {
        if (argc != 2) {
            print_usage(argv[0]);
            return 2;
        }

        auto bridge = WdaBridge::start(build_provider("wda-bridge"))
                          .expect("failed to start WDA bridge");
        auto endpoints = bridge.endpoints().expect("failed to read bridge endpoints");

        if (endpoints.udid.is_some()) {
            std::cout << "udid:        " << endpoints.udid.unwrap() << "\n";
        }
        std::cout << "wda_url:     " << endpoints.wda_url << "\n"
                  << "mjpeg_url:   " << endpoints.mjpeg_url << "\n"
                  << "device_http: " << endpoints.device_http << "\n"
                  << "device_mjpeg:" << endpoints.device_mjpeg << "\n";

        std::cout << "\nForwarding active. Press Enter to stop.\n";
        std::string line;
        std::getline(std::cin, line);
        return 0;
    }

    print_usage(argv[0]);
    return 2;
}
