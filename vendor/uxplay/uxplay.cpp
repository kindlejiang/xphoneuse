/**
 * RPiPlay - An open-source AirPlay mirroring server for Raspberry Pi
 * Copyright (C) 2019 Florian Draschbacher
 * Modified extensively to become 
 * UxPlay - An open-souce AirPlay mirroring server.
 * Modifications Copyright (C) 2021-23 F. Duncanh
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software Foundation,
 * Inc., 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301  USA
 */

#include <stddef.h>
#include <cstring>
#include <unistd.h>
#include <ctype.h>
#include <string>
#include <algorithm>
#include <vector>
#include <array>
#include <fstream>
#include <sstream>
#include <iostream>
#include <memory>
#include <iterator>
#include <sys/stat.h>
#include <cstdio>
#include <stdarg.h>
#include <math.h>
#include <inttypes.h>

#ifdef _WIN32  /*modifications for Windows compilation */
#include <glib.h>
#include <unordered_map>
#include <winsock2.h>
#include <iphlpapi.h>
#include <pthread.h>   //for pthreads in MSYS2 UCRT
#else
#include <csignal>
#include <glib-unix.h>
#include <sys/utsname.h>
#include <sys/socket.h>
#include <ifaddrs.h>
#include <sys/types.h>
#include <pwd.h>
# ifdef __linux__
# include <netpacket/packet.h>
# else
# include <net/if_dl.h>
#  ifdef __OpenBSD__
#  include <err.h>
#  endif
# endif
#endif

#include "lib/raop.h"
#include "lib/stream.h"
#include "lib/logger.h"
#include "lib/crypto.h"
#include "uxplay_api.h"


#define VERSION "1.74"

#define SECOND_IN_USECS 1000000
#define SECOND_IN_NSECS 1000000000UL
#define DEFAULT_NAME "UxPlay"
#define DEFAULT_DEBUG_LOG false
#define LOWEST_ALLOWED_PORT 1024
#define HIGHEST_PORT 65535
#define MISSED_FEEDBACK_LIMIT 15
#define MIN_PASSWORD_LENGTH 4
#define DEFAULT_PLAYBIN_VERSION 3
#define BT709_FIX "capssetter caps=\"video/x-h264, colorimetry=bt709\""
#define SRGB_FIX  " ! video/x-raw,colorimetry=sRGB,format=RGB  ! "
#ifdef FULL_RANGE_RGB_FIX
  #define DEFAULT_SRGB_FIX true
#else
  #define DEFAULT_SRGB_FIX false
#endif

static const char *appname = DEFAULT_NAME;
static std::string server_name = appname;
static bool server_name_is_utf8 = false;
static dnssd_t *dnssd = NULL;
static raop_t *raop = NULL;
static logger_t *render_logger = NULL;
static bool audio_sync = false;
static bool video_sync = true;
static bool relaunch_video = false;
static bool reset_loop = false;
static unsigned int open_connections= 0;
static bool use_video = true;
static unsigned char compression_type = 0;
static bool full_video_reset = true;
static bool show_client_FPS_data = false;
static FILE *video_dumpfile = NULL;
static std::string video_dumpfile_name = "videodump";
static int video_dump_limit = 0;
static int video_dumpfile_count = 0;
static int video_dump_count = 0;
static bool dump_video = false;
static unsigned char mark[] = { 0x00, 0x00, 0x00, 0x01 };
static bool do_append_hostname = true;
static bool use_random_hw_addr = false;
static unsigned short display[5] = {0}, tcp[3] = {0}, udp[3] = {0};
static bool debug_log = DEFAULT_DEBUG_LOG;
static bool suppress_packet_debug_data = false;
static int log_level = LOGGER_INFO;
static int nohold = 0;
static bool nofreeze = false;
static unsigned short raop_port;
static unsigned short airplay_port;
static uint64_t remote_clock_offset = 0;
static std::vector<std::string> allowed_clients;
static std::vector<std::string> blocked_clients;
static bool restrict_clients;
static bool setup_legacy_pairing = false;
static bool peer_to_peer = false;
static unsigned char pin_pw = 0;  /* 0: no client access control; 1: onscreen pin ; 2: require password (same password for all clients)  3: random pw*/
static std::string password = "";
static guint min_password_length = MIN_PASSWORD_LENGTH;
static unsigned short pin = 0;
static std::string keyfile = "";
static std::string mac_address = "";
static std::string dacpfile = "";
static bool registration_list = false;
static std::string pairing_register = "";
static std::vector <std::string> registered_keys;
static double db_low = -30.0;
static double db_high = 0.0;
static bool taper_volume = false;
static double initial_volume = 0.0;
static bool preserve_connections = false;
static guint missed_feedback_limit = MISSED_FEEDBACK_LIMIT;
static guint missed_feedback = 0;
static bool reset_httpd = false;
static GMainLoop *gmainloop = NULL;

/* --- "XPhoneUse" fork: FFI state ---------------------------------------- */

static uxplay_callbacks uxplay_cb = { NULL };
static bool uxplay_callbacks_set = false;
static GThread *uxplay_thread = NULL;
static bool uxplay_running = false;
static bool uxplay_quit_requested = false;

/* logging */

static void log(int level, const char* format, ...) {
    va_list vargs;
    if (level > log_level) return;
    switch (level) {
    case 0:
    case 1:
    case 2:
    case 3:
        printf("*** ERROR: ");
        break;
    case 4:
        printf("*** WARNING: ");
        break;
    default:
        break;
    }
    va_start(vargs, format);
    vprintf(format, vargs);
    printf("\n");
    va_end(vargs);
}

#define LOGD(...) log(LOGGER_DEBUG, __VA_ARGS__)
#define LOGI(...) log(LOGGER_INFO, __VA_ARGS__)
#define LOGW(...) log(LOGGER_WARNING, __VA_ARGS__)
#define LOGE(...) log(LOGGER_ERR, __VA_ARGS__)

#ifdef DBUS
static void dbus_screensaver_inhibiter(bool inhibit) {
    g_assert(inhibit != dbus_last_message);
    g_assert(scrsv);
    /* receive reply from previous request, whenever that was sent
     * (may have been sent hours ago ... !)
     * (code modeled on vlc/modules/misc/inhibit/dbus.c) */
    if (dbus_pending != NULL) {
        DBusMessage *reply;
        dbus_pending_call_block(dbus_pending);
        reply = dbus_pending_call_steal_reply(dbus_pending);
        dbus_pending_call_unref(dbus_pending);
        dbus_pending = NULL;
        if (reply != NULL) {
            if (!dbus_message_get_args(reply, NULL,
                                       DBUS_TYPE_UINT32, &dbus_cookie,
                                       DBUS_TYPE_INVALID)) {
                dbus_cookie = 0;
            }
            dbus_message_unref(reply);
        }
        LOGD("screen_saver: got D-Bus cookie %" PRIu32, (uint32_t) dbus_cookie);
    }

    if (!dbus_cookie && !inhibit) {
        return; /* nothing to do */
    }

    /* send request */
    const char *dbus_method = inhibit ? dbus_inhibit.c_str() : dbus_uninhibit.c_str();
    DBusMessage *dbus_message = dbus_message_new_method_call(dbus_service.c_str(),
                                                             dbus_path.c_str(),
                                                             dbus_interface.c_str(),
                                                             dbus_method);
    g_assert (dbus_message);

    if (inhibit) {
        dbus_bool_t ret;
        const char *reason = (scrsv == 1) ? reason_active : reason_always;

        ret = dbus_message_append_args(dbus_message,
                                       DBUS_TYPE_STRING, &appname,
                                       DBUS_TYPE_STRING, &reason,
                                       DBUS_TYPE_INVALID);
	g_assert(ret);

        ret =  dbus_connection_send_with_reply(dbus_connection, dbus_message, &dbus_pending, -1);
        if (!ret) {
            dbus_pending = NULL;
        }
    } else {
        g_assert(dbus_cookie);
        LOGD("screen_saver: releasing D-Bus cookie %" PRIu32, (uint32_t) dbus_cookie);
        if (dbus_message_append_args(dbus_message,
                                     DBUS_TYPE_UINT32, &dbus_cookie,
                                     DBUS_TYPE_INVALID)
            && dbus_connection_send(dbus_connection, dbus_message, NULL)) {
            dbus_cookie = 0;
        }
    }

    dbus_connection_flush(dbus_connection);
    dbus_message_unref(dbus_message);
    dbus_last_message = inhibit;
}
#endif

static char *create_pin_display(char *pin_str, int margin, int gap) {
    char *ptr;
    char num[2] = { 0 };
    int w = 10;
    int h = 8;
    char digits[10][8][11] = { "0821111380", "2114005113", "1110000111", "1110000111", "1110000111", "1110000111", "5113002114", "0751111470",
                               "0002111000", "0021111000", "0000111000", "0000111000", "0000111000", "0000111000", "0000111000", "0011111110", 
                               "0811112800", "2114005113", "0000000111", "0000082114", "0862111470", "2114700000", "1117000000", "1111111111", 
                               "0821111380", "2114005113", "0000082114", "0000111170", "0000075130", "1110000111", "5113002114", "0751111470", 
                               "0000211110", "0001401110", "0021401110", "0214001110", "2110001110", "1111111111", "0000001110", "0000001110", 
                               "1111111110", "1110000000", "1110000000", "1112111380", "0000075113", "0000000111", "5113002114", "0711114700",  
                               "0821111380", "2114005113", "1110000000", "1112111380", "1114075113", "1110000111", "5113002114", "0751111470", 
                               "1111111111", "0000002114", "0000021140", "0000211400", "0002114000", "0021140000", "0211400000", "2114000000", 
                               "0831111280", "2114002114", "5113802114", "0751111170", "8214775138", "1110000111", "5113002114", "0751111470", 
                               "0821111380", "2114005113", "1110000111", "5113802111", "0751114111", "0000000111", "5113002114", "0751111470"  
                             };

    char pixels[9] = { ' ', '8', 'd', 'b', 'P', 'Y', 'o', '"', '.' };
    /* Ascii art used here is derived from the FIGlet font "collosal" */

    int pin_val = (int) strtoul(pin_str, &ptr, 10);
    if (*ptr) {
        return NULL;
    }
    int len = strlen(pin_str);
    int *pin = (int *) calloc( len, sizeof(int));
    if(!pin) {
        return NULL;
    }

    for (int i = 0; i < len; i++) {
        pin[len - 1 - i] = pin_val % 10;
        pin_val = pin_val / 10;
    }
  
    int size = 4 + h*(margin + len*(w + gap + 1));
    char *pin_image = (char *) calloc(size, sizeof(char));
    if (!pin_image) {
        return NULL;
    }
    char *pos = pin_image;
    snprintf(pos, 2, "\n"); 
    pos++;

    for (int i = 0; i < h; i++) {
        for (int j = 0; j < margin; j++) {
            snprintf(pos, 2,  " ");
            pos++;
        }

        for (int j = 0; j < len; j++) {
            int l = pin[j];
            char *p = digits[l][i];
            for (int k = 0; k < w; k++) {
                char *ptr;
                strncpy(num, p++, 1);
                int r = (int) strtoul(num, &ptr, 10);
                snprintf(pos, 2, "%c", pixels[r]);
                pos++;
            }
            for (int n=0; n < gap ; n++) {
                snprintf(pos, 2, " ");
                pos++;
            }
        }
        snprintf(pos, 2, "\n");
        pos++;
    }
    snprintf(pos, 2, "\n");
    return pin_image;
}

static void dump_video_to_file(unsigned char *data, int datalen) {
    /*  SPS NAL has (data[4] & 0x1f) = 0x07  */
    if ((data[4] & 0x1f) == 0x07  && video_dumpfile && video_dump_limit) {
        fwrite(mark, 1, sizeof(mark), video_dumpfile);
        fclose(video_dumpfile);
        video_dumpfile = NULL;
        video_dump_count = 0;                     
    }

    if (!video_dumpfile) {
        std::string fn = video_dumpfile_name;
        if (video_dump_limit) {
            char suffix[20];
            video_dumpfile_count++;
            snprintf(suffix, sizeof(suffix), ".%d", video_dumpfile_count);
            fn.append(suffix);
        }
        fn.append(".h264");
        video_dumpfile = fopen (fn.c_str(),"w");
        if (video_dumpfile == NULL) {
            LOGE("could not open file %s for dumping h264 frames",fn.c_str());
        }
    }

    if (video_dumpfile) {
        if (video_dump_limit == 0) {
            fwrite(data, 1, datalen, video_dumpfile);
        } else if (video_dump_count < video_dump_limit) {
            video_dump_count++;
            fwrite(data, 1, datalen, video_dumpfile);
        }
    }
}

static gboolean feedback_callback(gpointer loop) {
    if (open_connections) {
        if (missed_feedback_limit && missed_feedback > missed_feedback_limit) {
            LOGI("***ERROR lost connection with client (network problem?)");
            LOGI("   Interval since last client feedback request exceeds limit of %u seconds", missed_feedback_limit);
            LOGI("   Sometimes the network connection may recover after a longer delay:\n"
                 "   the default limit n = %d seconds, can be changed with the \"-reset n\" option", MISSED_FEEDBACK_LIMIT);
            reset_httpd = true;
            relaunch_video = true;
            full_video_reset = true;
            g_main_loop_quit((GMainLoop *) loop);
            return TRUE;
        } else if (missed_feedback > 2) {
            LOGE("%3u seconds since last client feedback request (expected every two seconds); client may be offline", missed_feedback);
        }
        missed_feedback++;
    } else {
        missed_feedback = 0;
    }
    return TRUE;
}

static gboolean reset_callback(gpointer loop) {
    if (reset_loop) {
        g_main_loop_quit((GMainLoop *) loop);
    }
    return TRUE;
}

/* signals handlers (ctrl-c, etc )*/

static void cleanup();

#ifndef _WIN32
static gboolean sigint_callback(gpointer loop) {
    relaunch_video = false;
    g_main_loop_quit((GMainLoop *) loop);
    return TRUE;
}

static gboolean sigterm_callback(gpointer loop) {
    relaunch_video = false;
    g_main_loop_quit((GMainLoop *) loop);
    return TRUE;
}

static gboolean sighup_callback(gpointer loop) {
    relaunch_video = false;
    g_main_loop_quit((GMainLoop *) loop);
    return TRUE;
}
#endif

/* "XPhoneUse" fork: audio progress, HLS EOS and renderer bus watches are gone. */
static void main_loop()  {
    GMainLoop *loop = g_main_loop_new(NULL,FALSE);
    if (uxplay_quit_requested) {
        /* uxplay_stop() was called before the loop started running */
        g_main_loop_quit(loop);
    }
    relaunch_video = false;
    reset_loop = false;
    reset_httpd = false;
    preserve_connections = false;
    if (use_video) {
        relaunch_video = true;
    }

    missed_feedback = 0;
    guint feedback_watch_id = g_timeout_add_seconds(1, (GSourceFunc) feedback_callback, (gpointer) loop);
    guint reset_watch_id = g_timeout_add(100, (GSourceFunc) reset_callback, (gpointer) loop);

    /* uxplay_stop() quits this loop through gmainloop; must be set on all
     * platforms (was Win32-only, which hung g_thread_join elsewhere). */
    gmainloop = loop;
#ifndef _WIN32
    guint sigterm_watch_id = g_unix_signal_add(SIGTERM, (GSourceFunc) sigterm_callback, (gpointer) loop);
    guint sigint_watch_id = g_unix_signal_add(SIGINT, (GSourceFunc) sigint_callback, (gpointer) loop);
    guint sighup_watch_id = g_unix_signal_add(SIGHUP, (GSourceFunc) sigint_callback, (gpointer) loop);
#endif
    g_main_loop_run(loop);

    gmainloop = NULL;
#ifndef _WIN32
    if (sigint_watch_id > 0) g_source_remove(sigint_watch_id);
    if (sigterm_watch_id > 0) g_source_remove(sigterm_watch_id);
    if (sighup_watch_id > 0) g_source_remove(sighup_watch_id);
#endif

    if (reset_watch_id > 0) g_source_remove(reset_watch_id);
    if (feedback_watch_id > 0) g_source_remove(feedback_watch_id);
    g_main_loop_unref(loop);
}

static int parse_hw_addr (std::string str, std::vector<char> &hw_addr) {
    for (int i = 0; i < (int) str.length(); i += 3) {
        hw_addr.push_back((char) stol(str.substr(i), NULL, 16));
    }
    return 0;
}

static const char *get_homedir() {
    const char *homedir = getenv("XDG_CONFIG_HOMEDIR");
    if (homedir == NULL) {
        homedir = getenv("HOME");
    }
#ifndef _WIN32
    if (homedir == NULL){
        homedir = getpwuid(getuid())->pw_dir;
    }
#endif
    return homedir;
}

static std::string find_mac () {
/*  finds the MAC address of a network interface *
 *  in a Windows, Linux, *BSD or macOS system.   */
    std::string mac = "";
    char str[3];
#ifdef _WIN32
    ULONG buflen = sizeof(IP_ADAPTER_ADDRESSES);
    PIP_ADAPTER_ADDRESSES addresses = (IP_ADAPTER_ADDRESSES*) malloc(buflen);
    if (addresses == NULL) { 					
        return mac;
    }
    if (GetAdaptersAddresses(AF_UNSPEC, 0, NULL, addresses, &buflen) == ERROR_BUFFER_OVERFLOW) {
        free(addresses);
        addresses = (IP_ADAPTER_ADDRESSES*) malloc(buflen);
        if (addresses == NULL) {
            return mac;
        }
    }
    if (GetAdaptersAddresses(AF_UNSPEC, 0, NULL, addresses, &buflen) == NO_ERROR) {
        for (PIP_ADAPTER_ADDRESSES address = addresses; address != NULL; address = address->Next) {
            if (address->PhysicalAddressLength != 6                 /* MAC has 6 octets */
                || (address->IfType != 6 && address->IfType != 71)  /* Ethernet or Wireless interface */
                || address->OperStatus != 1) {                      /* interface is up */
                continue;
            }
            mac.erase();
            for (int i = 0; i < 6; i++) {
                snprintf(str, sizeof(str), "%02x", int(address->PhysicalAddress[i]));
                mac = mac + str;
                if (i < 5) mac = mac + ":";
            }
	    break;
        }
    }
    free(addresses);
    return mac;
#else
    struct ifaddrs *ifap, *ifaptr;
    int non_null_octets = 0;
    unsigned char octet[6];
    if (getifaddrs(&ifap) == 0) {
        for(ifaptr = ifap; ifaptr != NULL; ifaptr = ifaptr->ifa_next) {
            if(ifaptr->ifa_addr == NULL) continue;
#ifdef __linux__
            if (ifaptr->ifa_addr->sa_family != AF_PACKET) continue;
            struct sockaddr_ll *s = (struct sockaddr_ll*) ifaptr->ifa_addr;
            for (int i = 0; i < 6; i++) {
                if ((octet[i] = s->sll_addr[i]) != 0) non_null_octets++;
            }
#else    /* macOS and *BSD */
            if (ifaptr->ifa_addr->sa_family != AF_LINK) continue;
            unsigned char *ptr = (unsigned char *) LLADDR((struct sockaddr_dl *) ifaptr->ifa_addr);
            for (int i= 0; i < 6 ; i++) {
                if ((octet[i] = *ptr) != 0) non_null_octets++;
                ptr++;
            }
#endif
            if (non_null_octets) {
                mac.erase();
                for (int i = 0; i < 6 ; i++) {
                    snprintf(str, sizeof(str), "%02x", octet[i]);
                    mac = mac + str;
                    if (i < 5) mac = mac + ":";
                }
                break;
            }
        }
    }
    freeifaddrs(ifap);
#endif
    return mac;
}

static std::string random_mac () {
    char str[4];
    unsigned char random[6];
    get_random_bytes(random, sizeof(random));
    /* mark MAC address as locally administered, i.e. random */
    random[0] = random[0] & ~0x01;
    random[0] = random[0] | 0x02;
    snprintf(str,3,"%2.2x", random[0]);
    std::string mac_address(str);
    for (int i = 1; i < 6; i++) {
        snprintf(str,4,":%2.2x", random[i]);
        mac_address = mac_address + str;
    }
    return mac_address;
}

static void print_info (char *name) {
    printf("UxPlay %s: An Open-Source AirPlay mirroring server.\n", VERSION);
    printf("'XPhoneUse' fork: headless mirror server, video delivered via callback.\n");
    printf("-n name   Set AirPlay receiver name (default %s)\n", DEFAULT_NAME);
    printf("-fps n    Max frames per second (default 60)\n");
    printf("-nohold   Allow a new client to take over an existing session\n");
    printf("-vdmp n fn Dump received video to file (debug); n=0 unlimited\n");
    printf("-d        Debug logging\n");
    printf("-h        This help\n");
}


static bool option_has_value(const int i, const int argc, std::string option, const char *next_arg) {
    if (i >= argc - 1 || next_arg[0] == '-') {
        LOGE("invalid: \"%s\" had no argument", option.c_str());
        return false;
     }
    return true;
}

static bool get_value (const char *str, unsigned int *n) {
    // if n > 0 str must be a positive decimal <= input value *n  
    // if n = 0, str must be a non-negative decimal
    if (strlen(str) == 0 || strlen(str) > 10 || str[0] == '-') return false;
    char *end;
    unsigned long l = strtoul(str, &end, 10);
    if (*end) return false;
    if (*n && (l == 0 || l > *n)) return false;
    *n = (unsigned int) l;
    return true;
}

static void append_hostname(std::string &server_name) {
    std::string hostname;
#ifdef _WIN32   /*modification for compilation on Windows */
    char buffer[256] = "";
    unsigned long size = sizeof(buffer);
    if (GetComputerNameA(buffer, &size)) {
        hostname.append(buffer);
    }
#else
    struct utsname buf;
    if (!uname(&buf)) {
        hostname.append(buf.nodename);
    }
#endif
    /* remove any ".local" suffix (causes namespace problem on macOS when using a custom mDNSResponder)*/
    std::string dot_local = ".local";
    size_t pos = hostname.find(dot_local);
    if (pos != std::string::npos) {
        hostname.erase(pos);
    }
    server_name.append("@");
    server_name.append(hostname);
}

bool is_utf8(const char *string, bool *is_printable_ascii) {
    /* test if C-string is printable ascii or valid UTF-8 (max 4 bytes) */
    if (is_printable_ascii)  {
        *is_printable_ascii = true;
    }
    int len = (int) strlen(string);
    for (int i = 0; i < len; i++) {
        unsigned char c = (unsigned char) string[i];
        int n = 0;
        if (0x20 <= c && c <= 0x7e) {
            continue;   //printable ascii, no control characters.
        } else if (is_printable_ascii) {
            *is_printable_ascii = false;
        }
        if (0x00 <= c && c <= 0x7f) {
            continue; //one byte code, 0bbbbbbb
        } else if (c == 0xc0 || c == 0xc1) {
             return false;  //two byte code, invalid start byte
        } else if ((c & 0xe0) == 0xc0) {
            n = 1; //two byte code, 110bbbbb
        } else if (c == 0xe0 && i < len - 1 && (unsigned char) string[i + 1] < 0xa0) {
            return false;  //three byte code, overlong encoding
        } else if (c == 0xed && i < len - 1 && (unsigned char) string[i + 1] > 0x9f) {
            return false;  //three byte code, exclude U+dc00 to U+dfff
        } else if ((c & 0xf0) == 0xe0) {
            n = 2; //three byte code 1110bbbb
        } else if (c >= 0xf5) {
            return false;  //four byte code, invalid start byte
        } else if (c == 0xf0 && i < len - 1 && (unsigned char) string[i + 1] < 0x90) {
            return false;  //four byte code, overlong encoding
        } else if (c == 0xf4 && i < len - 1 && (unsigned char) string[i + 1] > 0x8f) {
            return false;  //four byte code, out of range character (> U+10ffff)
        } else if ((c & 0xf8) == 0xf0) {
            n = 3; //four byte code, 11110bbb
        } else {
            return false;  //more than 4 bytes
        }
        for (int j = 0; j < n && i < len ; j++) { // n bytes matching 10bbbbbb must follow ?
            if ((++i == len) || (((unsigned char) string[i] & 0xc0) != 0x80)) {
                return false;
            }
        }
	
    }
    return true;
}

static void parse_arguments (int argc, char *argv[]) {
    /* "XPhoneUse" fork: minimal CLI for the exe build. The DLL build uses
     * uxplay_start() with an explicit config struct instead. */
    for (int i = 1; i < argc; i++) {
        std::string arg(argv[i]);
        if (arg == "-n") {
            if (!option_has_value(i, argc, arg, argv[i + 1])) {
                fprintf(stderr, "option \"-n\" must be followed by a server name\n");
                exit(1);
            }
            server_name.erase();
            server_name.append(argv[++i]);
            server_name_is_utf8 = is_utf8(server_name.c_str(), NULL);
        } else if (arg == "-fps") {
            unsigned int n = 0;
            if (!option_has_value(i, argc, arg, argv[i + 1]) || !get_value(argv[++i], &n)) {
                fprintf(stderr, "option \"-fps\" must be followed by an integer > 0\n");
                exit(1);
            }
            display[3] = (unsigned short) n;
        } else if (arg == "-nohold") {
            nohold = 1;
        } else if (arg == "-vdmp") {
            dump_video = true;
            if (i < argc - 1 && *argv[i + 1] != '-') {
                unsigned int n = 0;
                if (get_value(argv[++i], &n)) {
                    if (n == 0) {
                        fprintf(stderr, "invalid \"-vdmp 0 %s\"; -vdmp n needs a non-zero value of n\n", argv[i]);
                        exit(1);
                    }
                    video_dump_limit = n;
                    if (option_has_value(i, argc, arg, argv[i + 1])) {
                        video_dumpfile_name.erase();
                        video_dumpfile_name.append(argv[++i]);
                    }
                } else {
                    i--;
                    video_dump_limit = 0;
                }
            } else {
                video_dump_limit = 0;
            }
        } else if (arg == "-d" || arg == "-debug") {
            debug_log = true;
        } else if (arg == "-h" || arg == "--help") {
            print_info(argv[0]);
            exit(0);
        } else if (arg == "-p2p") {
            /* macOS AWDL peer-to-peer discovery: unsupported in this fork */
            fprintf(stderr, "option -p2p is not supported in this build\n");
            exit(1);
        } else {
            fprintf(stderr, "unknown option \"%s\"; use -h for help\n", argv[i]);
            exit(1);
        }
    }
}


static int register_dnssd() {
    int dnssd_error;
    int dnssd_type = 0;
    uint64_t features;
    
    dnssd_error = dnssd_register_raop(dnssd, raop_port);
    if (dnssd_error) {
        LOGE("dnssd_register_raop failed with error code %d", dnssd_error);
        dnssd_error_text(&dnssd_error, appname);
        return -3;
    }

    dnssd_error = dnssd_register_airplay(dnssd, airplay_port);
    if (dnssd_error) {
        LOGE("dnssd_register_airplay failed with error code %d", dnssd_error);
        dnssd_error_text(&dnssd_error, appname);
        return -4;
    }

    LOGD("register_dnssd: advertised AirPlay service with \"Features\" code = 0x%llX",
         dnssd_get_airplay_features(dnssd));
    return 0;
}

static void unregister_dnssd() {
    if (dnssd) {
        dnssd_unregister_raop(dnssd);
        dnssd_unregister_airplay(dnssd);
    }
    return;
}

static void stop_dnssd() {
    if (dnssd) {
        unregister_dnssd();
        dnssd_destroy(dnssd);
        dnssd = NULL;
	return;
    }	
}

static int start_dnssd(std::vector<char> hw_addr, std::string name) {
    int dnssd_error;
    if (dnssd) {
        LOGE("start_dnssd error: dnssd != NULL");
        return 2;
    }
    /* pin_pw controls client access
       pin_pw = 1: client must enter pin displayed onscreen (first access only, remembered by client, coupled to UxPlay's deviceID)
              = 2: client must enter password (same password for all clients)
              = 3: client must enter randomly selected 4-digit password displayed like an on-screen pin (every access)
              = 0: no access control
    */
    dnssd = dnssd_init(name.c_str(), strlen(name.c_str()), hw_addr.data(), hw_addr.size(), pin_pw, &dnssd_error);
    if (dnssd_error) {
        LOGE("Could not initialize dnssd library!: error %d", dnssd_error);
        return 1;
    }

#if defined(__APPLE__) && defined(UXPLAY_HAVE_APPLE_P2P)
    /* support for p2p (macOS only) */
    dnssd_set_peer_to_peer(dnssd, (int) peer_to_peer);
#endif
    
    /* after dnssd starts, reset the default feature set here 
     * (overwrites features set in dnssdint.h)
     * default: FEATURES_1 = 0x5A7FFEE6, FEATURES_2 = 0 */

    dnssd_set_airplay_features(dnssd,  0, 0); // AirPlay video supported 
    dnssd_set_airplay_features(dnssd,  1, 1); // photo supported 
    dnssd_set_airplay_features(dnssd,  2, 1); // video protected with FairPlay DRM 
    dnssd_set_airplay_features(dnssd,  3, 0); // volume control supported for videos

    dnssd_set_airplay_features(dnssd,  4, 0); // http live streaming (HLS) supported
    dnssd_set_airplay_features(dnssd,  5, 1); // slideshow supported 
    dnssd_set_airplay_features(dnssd,  6, 1); // 
    dnssd_set_airplay_features(dnssd,  7, 1); // mirroring supported

    dnssd_set_airplay_features(dnssd,  8, 0); // screen rotation  supported 
    dnssd_set_airplay_features(dnssd,  9, 1); // audio supported 
    dnssd_set_airplay_features(dnssd, 10, 1); //  
    dnssd_set_airplay_features(dnssd, 11, 1); // audio packet redundancy supported

    dnssd_set_airplay_features(dnssd, 12, 1); // FaiPlay secure auth supported 
    dnssd_set_airplay_features(dnssd, 13, 1); // photo preloading  supported 
    dnssd_set_airplay_features(dnssd, 14, 1); // Authentication bit 4:  FairPlay authentication
    dnssd_set_airplay_features(dnssd, 15, 1); // Metadata bit 1 support:   Artwork 

    dnssd_set_airplay_features(dnssd, 16, 1); // Metadata bit 2 support:  Soundtrack  Progress 
    dnssd_set_airplay_features(dnssd, 17, 1); // Metadata bit 0 support:  Text (DAACP) "Now Playing" info.
    dnssd_set_airplay_features(dnssd, 18, 1); // Audio format 1 support:   
    dnssd_set_airplay_features(dnssd, 19, 1); // Audio format 2 support: must be set for AirPlay 2 multiroom audio 

    dnssd_set_airplay_features(dnssd, 20, 1); // Audio format 3 support: must be set for AirPlay 2 multiroom audio 
    dnssd_set_airplay_features(dnssd, 21, 1); // Audio format 4 support:
    dnssd_set_airplay_features(dnssd, 22, 1); // Authentication type 4: FairPlay authentication
    dnssd_set_airplay_features(dnssd, 23, 0); // Authentication type 1: RSA Authentication

    dnssd_set_airplay_features(dnssd, 24, 0); // 
    dnssd_set_airplay_features(dnssd, 25, 1); // 
    dnssd_set_airplay_features(dnssd, 26, 0); // Has Unified Advertiser info
    dnssd_set_airplay_features(dnssd, 27, 1); // Supports Legacy Pairing

    dnssd_set_airplay_features(dnssd, 28, 1); //  
    dnssd_set_airplay_features(dnssd, 29, 0); // 
    dnssd_set_airplay_features(dnssd, 30, 1); // RAOP support: with this bit set, the AirTunes service is not required. 
    dnssd_set_airplay_features(dnssd, 31, 0); // 


    /*  bits 32-63: see  https://emanualcozzi.net/docs/airplay2/features 
    dnssd_set_airplay_features(dnssd, 32, 0); // isCarPlay when ON,; Supports InitialVolume when OFF
    dnssd_set_airplay_features(dnssd, 33, 0); // Supports Air Play Video Play Queue
    dnssd_set_airplay_features(dnssd, 34, 0); // Supports Air Play from cloud (requires that bit 6 is ON)
    dnssd_set_airplay_features(dnssd, 35, 0); // Supports TLS_PSK

    dnssd_set_airplay_features(dnssd, 36, 0); //
    dnssd_set_airplay_features(dnssd, 37, 0); //
    dnssd_set_airplay_features(dnssd, 38, 0); //  Supports Unified Media Control (CoreUtils Pairing and Encryption)
    dnssd_set_airplay_features(dnssd, 39, 0); //

    dnssd_set_airplay_features(dnssd, 40, 0); // Supports Buffered Audio
    dnssd_set_airplay_features(dnssd, 41, 0); // Supports PTP
    dnssd_set_airplay_features(dnssd, 42, 0); // Supports Screen Multi Codec (allows h265 video)
    dnssd_set_airplay_features(dnssd, 43, 0); // Supports System Pairing

    dnssd_set_airplay_features(dnssd, 44, 0); // is AP Valeria Screen Sender
    dnssd_set_airplay_features(dnssd, 45, 0); //
    dnssd_set_airplay_features(dnssd, 46, 0); // Supports HomeKit Pairing and Access Control
    dnssd_set_airplay_features(dnssd, 47, 0); //

    dnssd_set_airplay_features(dnssd, 48, 0); // Supports CoreUtils Pairing and Encryption
    dnssd_set_airplay_features(dnssd, 49, 0); //
    dnssd_set_airplay_features(dnssd, 50, 0); // Metadata bit 3: "Now Playing" info sent by bplist not DAACP test
    dnssd_set_airplay_features(dnssd, 51, 0); // Supports Unified Pair Setup and MFi Authentication

    dnssd_set_airplay_features(dnssd, 52, 0); // Supports Set Peers Extended Message
    dnssd_set_airplay_features(dnssd, 53, 0); //
    dnssd_set_airplay_features(dnssd, 54, 0); // Supports AP Sync
    dnssd_set_airplay_features(dnssd, 55, 0); // Supports WoL

    dnssd_set_airplay_features(dnssd, 56, 0); // Supports Wol
    dnssd_set_airplay_features(dnssd, 57, 0); //
    dnssd_set_airplay_features(dnssd, 58, 0); // Supports Hangdog Remote Control
    dnssd_set_airplay_features(dnssd, 59, 0); // Supports AudioStreamConnection setup

    dnssd_set_airplay_features(dnssd, 60, 0); // Supports Audo Media Data Control         
    dnssd_set_airplay_features(dnssd, 61, 0); // Supports RFC2198 redundancy
    */

    /* "XPhoneUse" fork: HLS and h265 mirroring are not supported */
    dnssd_set_airplay_features(dnssd, 0, 0);
    dnssd_set_airplay_features(dnssd, 4, 0);
    dnssd_set_airplay_features(dnssd, 42, 0);

    /* bit 27 of Features determines whether the AirPlay2 client-pairing protocol will be used (1) or not (0) */
    dnssd_set_airplay_features(dnssd, 27, (int) setup_legacy_pairing);
    return 0;
}

static bool check_client(char *deviceid) {
    bool ret = false;
    int list =  allowed_clients.size();
    for (int i = 0; i < list ; i++) {
        if (!strcmp(deviceid,allowed_clients[i].c_str())) {
	    ret = true;
	    break;
        }
    }
    return ret;
}

static bool check_blocked_client(char *deviceid) {
    bool ret = false;
    int list =  blocked_clients.size();
    for (int i = 0; i < list ; i++) {
        if (!strcmp(deviceid,blocked_clients[i].c_str())) {
	    ret = true;
	    break;
        }
    }
    return ret;
}

// Server callbacks


//to be simplified

extern "C" void video_reset(void *cls, reset_type_t type) {
    switch (type) {
    case RESET_TYPE_NOHOLD:
        LOGD("video_reset: type = NoHold");
        preserve_connections = false;
        remote_clock_offset = 0;
        relaunch_video = true;
        break;
    case RESET_TYPE_HLS_EOS:
    case RESET_TYPE_RTP_TO_HLS_TEARDOWN:
    case RESET_TYPE_RTP_SHUTDOWN:
    case RESET_TYPE_HLS_SHUTDOWN:
    case RESET_TYPE_ON_VIDEO_PLAY:
        LOGD("video_reset: type = %d", (int) type);
        preserve_connections = true;
        remote_clock_offset = 0;
        relaunch_video = true;
        break;
    default:
        g_assert(FALSE);
        break;
    }
    reset_loop = true;
}

extern "C" int video_set_codec(void *cls, video_codec_t codec) {
    if (!use_video) {
        return 0;
    }
    if (uxplay_cb.on_video_codec) {
        uxplay_cb.on_video_codec(uxplay_cb.user_data, codec == VIDEO_CODEC_H265 ? UXPLAY_CODEC_H265 : UXPLAY_CODEC_H264);
    }
    return 0;
}

extern "C" void display_pin(void *cls, char *pin) {
    int margin = 10;
    int spacing = 3;
    char *image = create_pin_display(pin, margin, spacing);
    if (!image) {
        LOGE("create_pin_display could not create pin image, pin = %s", pin);
    } else {
        LOGI("%s\n",image);     
        free (image);
    }
}

extern "C" const char *passwd(void *cls, int *len){
    if (pin_pw == 2) {
        *len = password.size();
        return password.c_str();
    } else if  (pin_pw == 3) {
        *len = -1;
    } else {
        *len = 0;   /* no password used */
    }
    return NULL;
}

extern "C" void export_dacp(void *cls, const char *active_remote, const char *dacp_id) {
      if (dacpfile.length()) {
        FILE *fp = fopen(dacpfile.c_str(), "w");
        if (fp) {
            fprintf(fp,"%s\n%s\n", dacp_id, active_remote);
            fclose(fp);
        } else {
            LOGE("failed to open DACP export file \"%s\"", dacpfile.c_str());
        }
    }
}

extern "C" void conn_init (void *cls) {
    open_connections++;
    LOGD("Open connections: %i", open_connections);
}

extern "C" void conn_destroy (void *cls) {
    open_connections--;
    LOGD("Open connections: %i", open_connections);
    if (open_connections == 0) {
        remote_clock_offset = 0;
        if (dacpfile.length()) {
            remove (dacpfile.c_str());
        }
        if (uxplay_cb.on_disconnected) {
            uxplay_cb.on_disconnected(uxplay_cb.user_data);
        }
    }
}

extern "C" void conn_feedback (void *cls) {
    /* received client heartbeat signal: connection still exists */
    missed_feedback = 0;
}

extern "C" void conn_reset (void *cls, int reason) {
    switch (reason) {
    case 1:
        LOGI("*** ERROR lost connection with client (network problem?)");
	break;
    case 2:
        LOGI("*** ERROR Unsupported HLS streaming source: (exit attempt to stream)");
	break;
    default:
      break;
    }

    reset_httpd = true;
    relaunch_video = true;
    reset_loop = true;
}

extern "C" void report_client_request(void *cls, char *deviceid, char * model, char *name, bool * admit) {
    LOGI("connection request from %s (%s) with deviceID = %s\n", name, model, deviceid);
    if (restrict_clients) {
        *admit = check_client(deviceid);
        if (*admit == false) {
            LOGI("client connections have been restricted to those with listed deviceID,\nuse \"-allow %s\" to allow this client to connect.\n",
                 deviceid);
        }
    } else {
        *admit = true;
    }
    if (check_blocked_client(deviceid)) {
        *admit = false;
        LOGI("*** attempt to connect by blocked client (clientID %s): DENIED\n", deviceid);
    }
}

extern "C" void audio_process (void *cls, raop_ntp_t *ntp, audio_decode_struct *data) {
    /* "XPhoneUse" fork: audio is not decoded or forwarded. */
    (void) cls; (void) ntp; (void) data;
}

extern "C" void video_process (void *cls, raop_ntp_t *ntp, video_decode_struct *data) {
    if (dump_video) {
        dump_video_to_file(data->data, data->data_len);
    }
    if (!use_video) {
        return;
    }
    if (!remote_clock_offset) {
        uint64_t local_time = (data->ntp_time_local ? data->ntp_time_local : get_local_time());
        remote_clock_offset = local_time - data->ntp_time_remote;
    }
    data->ntp_time_remote = data->ntp_time_remote + remote_clock_offset;
    if (uxplay_cb.on_video_data) {
        uxplay_cb.on_video_data(uxplay_cb.user_data, data->data, data->data_len, data->nal_count, data->ntp_time_remote);
    }
}

extern "C" void video_pause (void *cls) {
    (void) cls;
}

extern "C" void video_resume (void *cls) {
    (void) cls;
}

extern "C" void audio_flush (void *cls) {
    (void) cls;
}

extern "C" void video_flush (void *cls) {
    (void) cls;
    if (uxplay_cb.on_video_flush) {
        uxplay_cb.on_video_flush(uxplay_cb.user_data);
    }
}

extern "C" double audio_set_client_volume(void *cls) {
    return initial_volume;
}

extern "C" void audio_set_volume (void *cls, float volume) {
    /* "XPhoneUse" fork: audio is not decoded or forwarded. */
    (void) cls; (void) volume;
}

extern "C" void audio_get_format (void *cls, unsigned char *ct, unsigned short *spf, bool *usingScreen, bool *isMedia, uint64_t *audioFormat) {
    /* "XPhoneUse" fork: audio is not decoded or forwarded. */
    (void) cls; (void) ct; (void) spf; (void) usingScreen; (void) isMedia; (void) audioFormat;
}

extern "C" void video_report_size(void *cls, float *width_source, float *height_source, float *width, float *height) {
    (void) cls;
    if (uxplay_cb.on_video_size) {
        uxplay_cb.on_video_size(uxplay_cb.user_data, *width, *height);
    }
    (void) width_source; (void) height_source;
}

extern "C" void audio_set_coverart(void *cls, const void *buffer, int buflen) {
    (void) cls; (void) buffer; (void) buflen;
}

extern "C" void audio_stop_coverart_rendering(void *cls) {
    (void) cls;
}

extern "C" void audio_set_progress(void *cls, uint32_t *start, uint32_t *curr, uint32_t *end) {
    (void) cls; (void) start; (void) curr; (void) end;
}

extern "C" void audio_set_metadata(void *cls, const void *buffer, int buflen) {
    (void) cls; (void) buffer; (void) buflen;
}

extern "C" void register_client(void *cls, const char *device_id, const char *client_pk, const char *client_name, const char *client_ip) {
    if (!registration_list) {
      /* we are not maintaining a list of registered clients */
        if (uxplay_cb.on_connected) {
            uxplay_cb.on_connected(uxplay_cb.user_data, device_id, client_name, client_ip);
        }
        return;
    }
    /* client_pk is NULL when the session had no fresh pair-setup (repeat
       connections via pair-verify) - register nothing in that case. */
    if (!client_pk) return;
    LOGI("registered new client: %s DeviceID = %s PK = \n%s", client_name, device_id, client_pk);
    registered_keys.push_back(client_pk);
    if (strlen(pairing_register.c_str())) {
        FILE *fp = fopen(pairing_register.c_str(), "a");
        if (fp) {
            fprintf(fp, "%s,%s,%s\n", client_pk, device_id, client_name);
            fclose(fp);
        }
    }
}

extern "C" bool check_register(void *cls, const char *client_pk) {
    if (!registration_list) {
        /* we are not maintaining a list of registered clients */
        return true;
    }
    LOGD("check returning client's pairing registration");
    std::string pk = client_pk;
    if (std::find(registered_keys.rbegin(), registered_keys.rend(), pk) != registered_keys.rend()) {
        LOGD("registration found: PK=%s", client_pk);
        return true;
    } else {
        LOGE("returning client's pairing registration not found: PK=%s", client_pk);
        return false;
    }
}

/* control callbacks for video player (HLS) - "XPhoneUse" fork: not supported */

extern "C" void on_video_play(void *cls, const char* location, const float start_position) {
    (void) cls; (void) location; (void) start_position;
}

extern "C" void on_video_scrub(void *cls, const float position) {
    (void) cls; (void) position;
}

extern "C" void on_video_rate(void *cls, const float rate) {
    (void) cls; (void) rate;
}

extern "C" float on_video_playlist_remove (void *cls) {
    (void) cls;
    return 0.0f;
}

extern "C" void on_video_stop(void *cls) {
    (void) cls;
}

extern "C" void on_video_acquire_playback_info (void *cls, playback_info_t *playback_info) {
    (void) cls;
    playback_info->duration = -1.0;
    playback_info->position = -1.0;
    playback_info->seek_start = 0.0;
    playback_info->seek_duration = 0.0;
    playback_info->rate = 1.0;
    playback_info->playback_buffer_empty = false;
    playback_info->playback_buffer_full = false;
    playback_info->ready_to_play = false;
    playback_info->playback_likely_to_keep_up = false;
}

extern "C" void log_callback (void *cls, int level, const char *msg) {
    switch (level) {
    case LOGGER_DEBUG:
        LOGD("%s", msg);
        break;
    case LOGGER_WARNING:
        LOGW("%s", msg);
        break;
    case LOGGER_INFO:
        LOGI("%s", msg);
        break;
    case LOGGER_ERR:
        LOGE("%s", msg);
        break;
    default:
        break;
    }
    if (uxplay_cb.on_log) {
        uxplay_cb.on_log(uxplay_cb.user_data, level, msg);
    }
}

static int start_raop_server (unsigned short display[5], unsigned short tcp[3], unsigned short udp[3], bool debug_log) {
    raop_callbacks_t raop_cbs;
    memset(&raop_cbs, 0, sizeof(raop_cbs));
    raop_cbs.conn_init = conn_init;
    raop_cbs.conn_destroy = conn_destroy;
    raop_cbs.conn_reset = conn_reset;
    raop_cbs.conn_feedback = conn_feedback;
    raop_cbs.audio_process = audio_process;
    raop_cbs.video_process = video_process;
    raop_cbs.audio_flush = audio_flush;
    raop_cbs.video_flush = video_flush;
    raop_cbs.video_pause = video_pause;
    raop_cbs.video_resume = video_resume;
    raop_cbs.audio_set_client_volume = audio_set_client_volume;
    raop_cbs.audio_set_volume = audio_set_volume;
    raop_cbs.audio_get_format = audio_get_format;
    raop_cbs.video_report_size = video_report_size;
    raop_cbs.audio_set_metadata = audio_set_metadata;
    raop_cbs.audio_set_coverart = audio_set_coverart;
    raop_cbs.audio_stop_coverart_rendering = audio_stop_coverart_rendering;
    raop_cbs.audio_set_progress = audio_set_progress;
    raop_cbs.report_client_request = report_client_request;
    raop_cbs.display_pin = display_pin;
    raop_cbs.register_client = register_client;
    raop_cbs.check_register = check_register;
    raop_cbs.passwd = passwd;
    raop_cbs.export_dacp = export_dacp;
    raop_cbs.video_reset = video_reset;
    raop_cbs.video_set_codec = video_set_codec;
#ifdef DBUS
    raop_cbs.mirror_video_running = mirror_video_running;
#endif
    raop_cbs.on_video_play = on_video_play;
    raop_cbs.on_video_scrub = on_video_scrub;
    raop_cbs.on_video_rate = on_video_rate;
    raop_cbs.on_video_stop = on_video_stop;
    raop_cbs.on_video_playlist_remove = on_video_playlist_remove;
    raop_cbs.on_video_acquire_playback_info = on_video_acquire_playback_info;

    raop = raop_init(&raop_cbs);
    if (raop == NULL) {
        LOGE("Error initializing raop!");
        return -1;
    }
    raop_set_log_callback(raop, log_callback, NULL);
    raop_set_log_level(raop, log_level);
    /* set nohold = 1 to allow  capture by new client */
    if (raop_init2(raop, nohold, mac_address.c_str(), keyfile.c_str())){
        LOGE("Error initializing raop (2)!");
        free (raop);
        return -1;
    }

    /* write desired display pixel width, pixel height, refresh_rate, max_fps, overscanned.  */
    /* use 0 for default values 1920,1080,60,30,0; these are sent to the Airplay client      */

    if (display[0]) raop_set_plist(raop, "width", (int) display[0]);
    if (display[1]) raop_set_plist(raop, "height", (int) display[1]);
    if (display[2]) raop_set_plist(raop, "refreshRate", (int) display[2]);
    if (display[3]) raop_set_plist(raop, "maxFPS", (int) display[3]);
    if (display[4]) raop_set_plist(raop, "overscanned", (int) display[4]);

    if (show_client_FPS_data) raop_set_plist(raop, "clientFPSdata", 1);
    if (pin_pw == 1) raop_set_plist(raop, "pin", (int) pin);

    /* network port selection (ports listed as "0" will be dynamically assigned) */
    raop_set_tcp_ports(raop, tcp);
    raop_set_udp_ports(raop, udp);

    raop_port = raop_get_port(raop);
    raop_start_httpd(raop, &raop_port);
    raop_set_port(raop, raop_port);

    /* use raop_port for airplay_port (instead of tcp[2]) */
    airplay_port = raop_port;

    if (dnssd) {
        raop_set_dnssd(raop, dnssd);
    } else {
        LOGE("raop_set failed to set dnssd");
        return -2;
    }
    return 0;
}

static void stop_raop_server () {
    if (raop) {
        raop_destroy(raop);
        raop = NULL;
    }
    return;
}

/* ------------------------------------------------------------------ */
/* "XPhoneUse" fork: server run function. Runs on the calling thread    */
/* until the glib main loop is quit (uxplay_stop, signal, or a        */
/* connection reset loop). Returns 0 on clean shutdown.               */

static int uxplay_run(const uxplay_config *config) {
    std::vector<char> server_hw_addr;

#ifdef _WIN32
    /* initialise Windows kernel qpc frequency for recv timestamping */
    ntp_global_init();
    /* use utf-8 terminal output; don't buffer stdout when debug_log = false */
    SetConsoleOutputCP(CP_UTF8);
    if (!debug_log) {
        setbuf(stdout, NULL);
    }
#endif

    use_video = true;
    uxplay_quit_requested = false;
    if (config->max_fps > 0) {
        display[3] = (unsigned short) config->max_fps;
    }
    nohold = config->nohold ? 1 : 0;
    if (config->server_name && strlen(config->server_name)) {
        server_name = config->server_name;
    }
    if (config->keyfile && strlen(config->keyfile)) {
        keyfile = config->keyfile;
    }

    if (do_append_hostname) {
        append_hostname(server_name);
    }

    render_logger = logger_init();
    logger_set_callback(render_logger, log_callback, NULL);
    logger_set_level(render_logger, log_level);

    if (!use_random_hw_addr) {
        if (strlen(mac_address.c_str()) == 0) {
            mac_address = find_mac();
            LOGI("using system MAC address %s", mac_address.c_str());
        } else {
            LOGI("using user-set MAC address %s", mac_address.c_str());
        }
    }
    if (mac_address.empty()) {
        mac_address = random_mac();
        LOGI("using randomly-generated MAC address %s", mac_address.c_str());
    }
    parse_hw_addr(mac_address, server_hw_addr);

    /* mirror resolution request (h264 only in this fork) */
    if (!display[0] && !display[1]) {
        display[0] = 1920;
        display[1] = 1080;
    }

    if (start_dnssd(server_hw_addr, server_name)) {
        cleanup();
        return -1;
    }
    if (start_raop_server(display, tcp, udp, debug_log)) {
        stop_dnssd();
        cleanup();
        return -1;
    }
    if (register_dnssd()) {
        stop_raop_server();
        stop_dnssd();
        cleanup();
        return -1;
    }

    LOGI("UxPlay %s ('Another' headless fork): AirPlay mirroring server ready as \"%s\"", VERSION, server_name.c_str());

reconnect:
    compression_type = 0;
    main_loop();
    if (relaunch_video) {
        if (reset_httpd) {
            raop_stop_httpd(raop);
        }
        if (!preserve_connections) {
            raop_remove_known_connections(raop);
        }
        full_video_reset = false;
        if (reset_httpd) {
            unsigned short port = raop_get_port(raop);
            raop_start_httpd(raop, &port);
            raop_set_port(raop, port);
        }
        goto reconnect;
    }

    LOGI("Stopping RAOP Server...");
    stop_raop_server();
    stop_dnssd();
    cleanup();
    return 0;
}

/* ------------------------------------------------------------------ */
/* C ABI                                                               */

static void *uxplay_run_thread(void *data) {
    uxplay_config *config = (uxplay_config *) data;
    int ret = uxplay_run(config);
    (void) ret;
    free((void *) config->server_name);
    free((void *) config->keyfile);
    delete config;
    uxplay_running = false;
    uxplay_thread = NULL;
    if (uxplay_cb.on_disconnected) {
        uxplay_cb.on_disconnected(uxplay_cb.user_data);
    }
    return NULL;
}

extern "C" int uxplay_start(const uxplay_config *config, const uxplay_callbacks *callbacks) {
    if (!config) {
        return -1;
    }
    if (uxplay_thread || uxplay_running) {
        LOGE("uxplay_start: server already running");
        return -2;
    }
    if (callbacks) {
        uxplay_cb = *callbacks;
        uxplay_callbacks_set = true;
    } else {
        memset(&uxplay_cb, 0, sizeof(uxplay_cb));
        uxplay_callbacks_set = false;
    }

    uxplay_config *copy = new uxplay_config(*config);
    copy->server_name = config->server_name ? strdup(config->server_name) : NULL;
    copy->keyfile = config->keyfile ? strdup(config->keyfile) : NULL;

    uxplay_running = true;
    uxplay_thread = g_thread_new("uxplay", uxplay_run_thread, copy);
    if (!uxplay_thread) {
        free((void *) copy->server_name);
        free((void *) copy->keyfile);
        delete copy;
        uxplay_running = false;
        return -3;
    }
    return 0;
}

extern "C" void uxplay_stop(void) {
    uxplay_quit_requested = true;
    if (gmainloop) {
        relaunch_video = false;
        g_main_loop_quit(gmainloop);
    }
    if (uxplay_thread) {
        g_thread_join(uxplay_thread);
        uxplay_thread = NULL;
    }
}

extern "C" int uxplay_is_running(void) {
    return uxplay_running ? 1 : 0;
}

/* ------------------------------------------------------------------ */
/* exe build: minimal CLI wrapper around uxplay_start                  */

#ifdef _WIN32
static BOOL WINAPI CtrlHandler(DWORD signal) {
    switch (signal) {
    case CTRL_C_EVENT:
    case CTRL_CLOSE_EVENT:
    case CTRL_SHUTDOWN_EVENT:
        uxplay_stop();
        return TRUE;
    default:
        return FALSE;
    }
}
#else
static void CtrlHandler(int signum) {
    (void) signum;
    uxplay_stop();
}
#endif

int main (int argc, char *argv[]) {
    parse_arguments(argc, argv);
    log_level = (debug_log ? LOGGER_DEBUG_DATA : LOGGER_INFO);

#ifdef _WIN32
    if (!SetConsoleCtrlHandler(CtrlHandler, TRUE)) {
        LOGE("Could not set control handler");
        return 1;
    }
#else
    signal(SIGINT, CtrlHandler);
    signal(SIGTERM, CtrlHandler);
    signal(SIGHUP, CtrlHandler);
#endif

    uxplay_config config;
    memset(&config, 0, sizeof(config));
    config.server_name = server_name.c_str();
    config.max_fps = (int) display[3];
    config.nohold = nohold;
    config.keyfile = NULL;

    if (uxplay_start(&config, NULL)) {
        fprintf(stderr, "failed to start uxplay server\n");
        return 1;
    }
    while (uxplay_is_running()) {
        g_usleep(100 * 1000);
    }
    return 0;
}

static void cleanup() {
    logger_destroy(render_logger);
    render_logger = NULL;
    if (video_dumpfile) {
        fwrite(mark, 1, sizeof(mark), video_dumpfile);
        fclose(video_dumpfile);
        video_dumpfile = NULL;
    }
}
