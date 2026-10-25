#!/usr/bin/env bash
# Builds StageTune as CLAP, AUv2 (for MainStage/Logic) and VST3.
# The AU and VST3 are made from the CLAP by clap-wrapper (wrappers/).
#   scripts/build.sh            build into target/bundled
#   scripts/build.sh --install  also copy to ~/Library/Audio/Plug-Ins and validate the AU
set -euo pipefail
cd "$(dirname "$0")/.."
source "$HOME/.cargo/env" 2>/dev/null || true

# Universal binary so the AU also loads in MainStage under Rosetta / on Intel Macs.
rustup target add x86_64-apple-darwin aarch64-apple-darwin >/dev/null
cargo xtask bundle-universal stagetune --release

if [ ! -d wrappers/clap-wrapper ]; then
  git clone --depth 1 https://github.com/free-audio/clap-wrapper.git wrappers/clap-wrapper
fi
# Fresh wrapper build every time: this regenerates the AU's Info.plist with
# its AudioComponents entry, which macOS uses to register the AU.
rm -rf wrappers/build
cmake -S wrappers -B wrappers/build -DCMAKE_BUILD_TYPE=Release -DCMAKE_OSX_ARCHITECTURES="arm64;x86_64" >/dev/null
cmake --build wrappers/build --config Release -j 8
if ! plutil -p wrappers/build/StageTune.component/Contents/Info.plist | grep -q AudioComponents; then
  echo "error: StageTune.component Info.plist has no AudioComponents entry" >&2
  exit 1
fi
for b in StageTune.component StageTune.vst3; do
  rm -rf "target/bundled/$b"
  cp -R "wrappers/build/$b" target/bundled/
  codesign --force --deep -s - "target/bundled/$b"
done

echo "Built:"
ls -d target/bundled/StageTune.{clap,component,vst3}

if [ "${1:-}" = "--install" ]; then
  P="$HOME/Library/Audio/Plug-Ins"
  mkdir -p "$P/Components" "$P/CLAP" "$P/VST3"
  rm -rf "$P/Components/StageTune.component" "$P/CLAP/StageTune.clap" "$P/VST3/StageTune.vst3"
  cp -R target/bundled/StageTune.component "$P/Components/"
  cp -R target/bundled/StageTune.clap "$P/CLAP/"
  cp -R target/bundled/StageTune.vst3 "$P/VST3/"
  # Make the AU host cache notice the new component.
  killall -9 AudioComponentRegistrar 2>/dev/null || true
  echo "Installed to $P. Waiting for the AU registry…"
  for _ in $(seq 1 20); do auval -a 2>/dev/null | grep -q StTn && break; sleep 1; done
  auval -v aumf StTn BHmm | tail -5
fi
