# Wraps the nih-plug CLAP build (target/bundled/StageTune.clap) into an AUv2
# .component (MainStage / Logic) and a VST3, using clap-wrapper (MIT) with
# Apple's AudioUnitSDK (Apache-2.0) and Steinberg's VST3 SDK 3.8 (MIT). The
# CLAP is embedded in each bundle, so they are self-contained.
#
# Built by scripts/build.sh; see the README.
cmake_minimum_required(VERSION 3.21)
project(StageTuneWrappers VERSION 0.1.0 LANGUAGES C CXX OBJC OBJCXX)

set(CMAKE_OSX_DEPLOYMENT_TARGET "11.0" CACHE STRING "")
set(CMAKE_CXX_STANDARD 17)
set(CLAP_WRAPPER_DOWNLOAD_DEPENDENCIES TRUE CACHE BOOL "")
set(CLAP_WRAPPER_DONT_ADD_TARGETS TRUE CACHE BOOL "")

set(STAGETUNE_CLAP "${CMAKE_CURRENT_SOURCE_DIR}/../target/bundled/StageTune.clap"
    CACHE PATH "The CLAP bundle to wrap")

add_subdirectory(clap-wrapper)

add_library(StageTune_auv2 MODULE)
target_add_auv2_wrapper(
    TARGET StageTune_auv2
    OUTPUT_NAME "StageTune"
    BUNDLE_IDENTIFIER "com.benjaminhamm.stagetune.auv2"
    BUNDLE_VERSION "${PROJECT_VERSION}"
    # 'aumf' = music effect: an audio effect that also receives MIDI, which
    # the MIDI target-note feature needs. It shows up under
    # "AU MIDI-controlled Effects" in MainStage/Logic.
    INSTRUMENT_TYPE "aumf"
    MANUFACTURER_NAME "Benjamin Hamm"
    MANUFACTURER_CODE "BHmm"
    SUBTYPE_CODE "StTn"
    MACOS_EMBEDDED_CLAP_LOCATION "${STAGETUNE_CLAP}"
)

add_library(StageTune_vst3 MODULE)
target_add_vst3_wrapper(
    TARGET StageTune_vst3
    OUTPUT_NAME "StageTune"
    BUNDLE_IDENTIFIER "com.benjaminhamm.stagetune.vst3"
    BUNDLE_VERSION "${PROJECT_VERSION}"
    MACOS_EMBEDDED_CLAP_LOCATION "${STAGETUNE_CLAP}"
)
