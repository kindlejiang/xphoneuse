cmake_minimum_required( VERSION 3.13 )

project( uxplay )

message( STATUS "Project name: " ${PROJECT_NAME} )

include(GNUInstallDirs)

set ( CMAKE_CXX_STANDARD 11 )

# "XPhoneUse" fork: GStreamer, audio, local rendering, X11, D-Bus and HLS
# support have been removed. The AirPlay mirror stream is delivered as
# decrypted Annex-B NAL data through the C ABI in uxplay_api.h.
# glib is still required (event loop + utility types used throughout lib/).

find_package(PkgConfig REQUIRED)
pkg_check_modules(GLIB REQUIRED glib-2.0)
include_directories(${GLIB_INCLUDE_DIRS})

if (USE_DNS_SD OR ( APPLE AND NOT USE_MDNS ))
  message ( STATUS "******** using Bonjour/Apple dns_sd.h for Service Discovery")
  if ( APPLE )
    add_definitions( -DUXPLAY_HAVE_APPLE_P2P )
  endif()
  add_subdirectory( lib/dns_sd)
  if (UNIX AND NOT APPLE)
    add_definitions( -DSUPPRESS_AVAHI_COMPAT_WARNING )
  endif()
  if ( APPLE )
       message ( STATUS "******** use \"cmake -DUSE_MDNS=1\" to build using UxPlay mdnsd code instead of Apple Bonjour" )
  endif()
else()
  message ( STATUS "******** using internal minimal mDNSResponder implementation (lib/mdnsd) for Service Discovery")
  if (NOT APPLE)
     message ( STATUS "******** use \"cmake -DUSE_DNS_SD=1\" to build with the previously-used Apple/Avahi dns-sd.h code")
  endif()
  add_subdirectory( lib/mdnsd)
endif()

add_subdirectory( lib/llhttp )
add_subdirectory( lib/playfair )
add_subdirectory( lib )

# One object library feeds both targets (exe for debugging, dll for embedding).
add_library( uxplay_objs OBJECT uxplay.cpp )
target_compile_definitions( uxplay_objs PRIVATE UXPLAY_BUILD_DLL )
target_link_libraries( uxplay_objs PUBLIC airplay )

add_executable( uxplay $<TARGET_OBJECTS:uxplay_objs> )
target_link_libraries( uxplay PRIVATE airplay ${GLIB_LIBRARIES} )

add_library( uxplay_dll SHARED $<TARGET_OBJECTS:uxplay_objs> )
target_link_libraries( uxplay_dll PRIVATE airplay ${GLIB_LIBRARIES} )
set_target_properties( uxplay_dll PROPERTIES
    OUTPUT_NAME uxplay
    PREFIX ""
    WINDOWS_EXPORT_ALL_SYMBOLS OFF )

install( TARGETS  uxplay RUNTIME DESTINATION bin )
install( TARGETS  uxplay_dll RUNTIME DESTINATION bin LIBRARY DESTINATION lib )

# uninstall target
if(NOT TARGET uninstall)
  configure_file(
    "${CMAKE_CURRENT_SOURCE_DIR}/cmake_uninstall.cmake.in"
    "${CMAKE_CURRENT_BINARY_DIR}/cmake_uninstall.cmake"
    IMMEDIATE @ONLY)

  add_custom_target(uninstall
    COMMAND ${CMAKE_COMMAND} -P ${CMAKE_CURRENT_BINARY_DIR}/cmake_uninstall.cmake)
endif()
