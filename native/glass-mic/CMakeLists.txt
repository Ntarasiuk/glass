# Glass Microphone: GlassMicrophone.driver, a Core Audio server plug-in (see Driver.cpp).
#   cmake -S native/glass-mic -B target/glass-mic && cmake --build target/glass-mic
# Universal (Apple Silicon + Intel), macOS 13+. libASPL (MIT) is fetched at a pinned commit.
cmake_minimum_required(VERSION 3.16)

set(CMAKE_OSX_ARCHITECTURES "arm64;x86_64" CACHE STRING "")
set(CMAKE_OSX_DEPLOYMENT_TARGET "13.0" CACHE STRING "")
project(GlassMicrophone CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

set(DRIVER_NAME "GlassMicrophone")
set(DRIVER_VERSION "1.0.0")
set(DRIVER_IDENTIFIER "dev.glassdesk.microphone")
# The plug-in factory's UUID (any fixed UUID of ours).
set(DRIVER_UID "6C1D7C2E-4B8A-4F0E-9E57-6F3B2A9C1D40")
set(DRIVER_ENTRYPOINT "GlassMicrophoneEntryPoint")

include(ExternalProject)
ExternalProject_Add(libASPL
  GIT_REPOSITORY https://github.com/gavv/libASPL.git
  GIT_TAG 633e0f70203edd87d320fc5a3cae901e1363aac5
  INSTALL_DIR ${CMAKE_CURRENT_BINARY_DIR}/libASPL-prefix
  LIST_SEPARATOR |
  CMAKE_ARGS
    -DCMAKE_INSTALL_PREFIX=<INSTALL_DIR>
    -DCMAKE_OSX_ARCHITECTURES=arm64|x86_64
    -DCMAKE_OSX_DEPLOYMENT_TARGET=${CMAKE_OSX_DEPLOYMENT_TARGET}
    -DCMAKE_BUILD_TYPE=Release
    -DBUILD_TESTING=OFF
  LOG_DOWNLOAD YES LOG_CONFIGURE YES LOG_BUILD YES LOG_INSTALL YES
)

add_library(${DRIVER_NAME} MODULE Driver.cpp)
add_dependencies(${DRIVER_NAME} libASPL)
target_include_directories(${DRIVER_NAME} PRIVATE ${CMAKE_CURRENT_BINARY_DIR}/libASPL-prefix/include)
target_link_libraries(${DRIVER_NAME} PRIVATE
  ${CMAKE_CURRENT_BINARY_DIR}/libASPL-prefix/lib/libASPL.a
  "-framework CoreFoundation")

set_target_properties(${DRIVER_NAME} PROPERTIES
  OUTPUT_NAME "${DRIVER_NAME}"
  BUNDLE true
  BUNDLE_EXTENSION "driver"
  PREFIX ""
  SUFFIX ""
  MACOSX_BUNDLE ON
  MACOSX_BUNDLE_INFO_PLIST "${CMAKE_CURRENT_SOURCE_DIR}/Info.plist.in"
  MACOSX_BUNDLE_BUNDLE_NAME "Glass Microphone"
  MACOSX_BUNDLE_BUNDLE_VERSION "${DRIVER_VERSION}"
  MACOSX_BUNDLE_COPYRIGHT "Glass (MIT); libASPL (MIT)"
  MACOSX_BUNDLE_GUI_IDENTIFIER "${DRIVER_IDENTIFIER}"
  MACOSX_BUNDLE_SHORT_VERSION_STRING "${DRIVER_VERSION}"
)
