#!/usr/bin/env bash
set -euo pipefail
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
profile=release
demo=false
for arg in "$@"; do
    case "$arg" in
        --debug) profile=debug ;;
        --demo) demo=true ;;
        *) echo "Usage: scripts/bundle.sh [--debug] [--demo]" >&2; exit 2 ;;
    esac
done
cd "$project_root"
if [[ "$profile" == release ]]; then
    cargo build --release -p rapid-mlx-ui
else
    cargo build -p rapid-mlx-ui
fi
app_name="Rapid MLX UI"
bundle_id="com.rapidmlx.ui"
if [[ "$demo" == true ]]; then
    app_name="Rapid MLX UI Demo"
    bundle_id="com.rapidmlx.ui.demo"
fi
app_dir="$project_root/dist/$app_name.app"
mkdir -p "$app_dir/Contents/MacOS" "$app_dir/Contents/Resources"
cp "target/$profile/rapid-mlx-ui" "$app_dir/Contents/MacOS/.rapid-mlx-ui.new"
mv -f "$app_dir/Contents/MacOS/.rapid-mlx-ui.new" "$app_dir/Contents/MacOS/rapid-mlx-ui"
executable=rapid-mlx-ui
demo_environment=""
if [[ "$demo" == true ]]; then demo_environment='<key>LSEnvironment</key><dict><key>RAPID_MLX_UI_DEMO</key><string>1</string></dict>'; fi
cat > "$app_dir/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>$app_name</string>
<key>CFBundleDisplayName</key><string>$app_name</string>
<key>CFBundleIdentifier</key><string>$bundle_id</string>
<key>CFBundleExecutable</key><string>$executable</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSMinimumSystemVersion</key><string>27.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSMicrophoneUsageDescription</key><string>Record audio when you press Record for local transcription on your Mac.</string>
$demo_environment
</dict></plist>
PLIST
plutil -lint "$app_dir/Contents/Info.plist"
echo "Created $app_dir"
