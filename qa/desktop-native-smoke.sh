#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
desktop_root="$repository_root/desktop"
binary_path="$desktop_root/src-tauri/target/debug/aworkit-desktop"

temporary_directory=$(mktemp -d)
application_pid=""
broadway_pid=""
driver_pid=""
webdriver_session_id=""
cleanup_native_smoke() {
  if [[ -n "$webdriver_session_id" && -n "$driver_pid" ]]; then
    curl --silent --request DELETE \
      "http://127.0.0.1:$driver_port/session/$webdriver_session_id" >/dev/null || true
  fi
  for process_id in "$application_pid" "$driver_pid" "$broadway_pid"; do
    if [[ -n "$process_id" ]] && kill -0 "$process_id" 2>/dev/null; then
      kill "$process_id" 2>/dev/null || true
      wait "$process_id" 2>/dev/null || true
    fi
  done
  rm -r -- "$temporary_directory"
}
trap cleanup_native_smoke EXIT

(
  cd "$desktop_root"
  ./node_modules/.bin/tsc --noEmit
  ./node_modules/.bin/vite build
  ./node_modules/.bin/tauri build --debug --no-bundle --ci \
    --config '{"build":{"beforeBuildCommand":""}}' \
    2>&1 | tee "$temporary_directory/tauri-build.log"
)

if ! grep -Fq "Built application at: $binary_path" \
  "$temporary_directory/tauri-build.log"; then
  echo "Tauri did not select the aworkit-desktop binary" >&2
  grep -F "Built application at:" "$temporary_directory/tauri-build.log" >&2 || true
  exit 1
fi
if grep -F "Built application at:" "$temporary_directory/tauri-build.log" \
  | grep -Fq "aworkit-rescue-e2e"; then
  echo "Tauri selected the rescue test runner instead of aworkit-desktop" >&2
  exit 1
fi
test -x "$binary_path"
if find "$desktop_root/src" "$desktop_root/dist" -type f -newer "$binary_path" -print -quit | grep -q .; then
  echo "native binary is older than desktop source or bundled frontend assets" >&2
  exit 1
fi

validate_native_screenshot() {
  local screenshot_path=$1
  local mode=$2
  python3 - "$screenshot_path" "$mode" <<'PY'
from collections import Counter
from pathlib import Path
import sys
from PIL import Image

path = Path(sys.argv[1])
mode = sys.argv[2]
image = Image.open(path).convert("RGB")
if image.width < 760 or image.height < 560:
    raise SystemExit(f"desktop screenshot is too small: {image.width}x{image.height}")
left = image.width // 10
top = image.height // 10
sample = image.crop((left, top, image.width - left, image.height - top)).resize((320, 200))
colors = Counter(sample.getdata())
dominant_fraction = colors.most_common(1)[0][1] / (sample.width * sample.height)
minimum_colors = 900 if mode == "broadway" else 32
maximum_dominant = 0.65 if mode == "broadway" else 0.82
if len(colors) < minimum_colors:
    raise SystemExit(f"desktop screenshot has only {len(colors)} colors and appears incomplete")
if dominant_fraction >= maximum_dominant:
    raise SystemExit(
        f"desktop screenshot is {dominant_fraction:.1%} one color and resembles the stale placeholder"
    )
print(
    f"native WebView screenshot {image.width}x{image.height}; "
    f"colors={len(colors)}; dominant={dominant_fraction:.1%}; backend={mode}"
)
PY
}

# Use a clean Broadway display even when a user session exists. A screenshot of
# an existing desktop could look nonblank while Aworkit's own WebView is blank.
  for command in broadwayd firefox geckodriver curl jq base64 python3; do
    if ! command -v "$command" >/dev/null; then
      echo "native WebView QA requires the isolated Broadway dependency $command" >&2
      exit 1
    fi
  done
  if ! python3 -c 'import PIL' >/dev/null 2>&1; then
    echo "native WebView QA requires the Python Pillow package" >&2
    exit 1
  fi
  broadway_port=$((18000 + ($$ % 18000)))
  driver_port=$((broadway_port + 1))
  broadway_display=$((400 + ($$ % 2000)))
  broadwayd --address=127.0.0.1 --port="$broadway_port" \
    ":$broadway_display" >"$temporary_directory/broadway.log" 2>&1 &
  broadway_pid=$!
  geckodriver --port "$driver_port" >"$temporary_directory/geckodriver.log" 2>&1 &
  driver_pid=$!
  for _ in {1..60}; do
    if curl --fail --silent "http://127.0.0.1:$broadway_port/" >/dev/null \
      && curl --fail --silent "http://127.0.0.1:$driver_port/status" >/dev/null; then
      break
    fi
    sleep 0.1
  done
  curl --fail --silent "http://127.0.0.1:$broadway_port/" >/dev/null
  curl --fail --silent "http://127.0.0.1:$driver_port/status" >/dev/null

  GDK_BACKEND=broadway \
    BROADWAY_DISPLAY=":$broadway_display" \
    XDG_DATA_HOME="$temporary_directory/xdg-data" \
    XDG_CONFIG_HOME="$temporary_directory/xdg-config" \
    WEBKIT_DISABLE_COMPOSITING_MODE=1 \
    "$binary_path" >"$temporary_directory/aworkit.log" 2>&1 &
  application_pid=$!
  session_response=$(curl --fail --silent --request POST \
    --header 'Content-Type: application/json' \
    --data '{"capabilities":{"alwaysMatch":{"moz:firefoxOptions":{"args":["-headless"]}}}}' \
    "http://127.0.0.1:$driver_port/session")
  webdriver_session_id=$(jq -er '.value.sessionId' <<<"$session_response")
  webdriver="http://127.0.0.1:$driver_port/session/$webdriver_session_id"
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data '{"width":1600,"height":1100}' "$webdriver/window/rect" >/dev/null
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data "{\"url\":\"http://127.0.0.1:$broadway_port/\"}" "$webdriver/url" >/dev/null
  for _ in {1..60}; do
    canvas_ready=$(curl --fail --silent --request POST \
      --header 'Content-Type: application/json' \
      --data '{"script":"const canvas=document.querySelector(\"canvas\"); return canvas!==null && canvas.width>=1400 && canvas.height>=900;","args":[]}' \
      "$webdriver/execute/sync" | jq -r '.value')
    [[ "$canvas_ready" == "true" ]] && break
    sleep 0.2
  done
  [[ "$canvas_ready" == "true" ]]
  if ! kill -0 "$application_pid" 2>/dev/null; then
    sed -n '1,200p' "$temporary_directory/aworkit.log" >&2
    echo "native Aworkit process exited before the Broadway WebView gate" >&2
    exit 1
  fi
  screenshot_path="$temporary_directory/aworkit-broadway.png"
  rendered=false
  for _ in {1..40}; do
    curl --fail --silent "$webdriver/screenshot" \
      | jq -er '.value' \
      | base64 --decode >"$screenshot_path"
    if validate_native_screenshot "$screenshot_path" broadway \
      >"$temporary_directory/validation.log" 2>&1; then
      rendered=true
      break
    fi
    sleep 0.25
  done
  if [[ "$rendered" != "true" ]]; then
    sed -n '1,120p' "$temporary_directory/validation.log" >&2
    exit 1
  fi
  cat "$temporary_directory/validation.log"
  echo "native Aworkit WebView rendered through the headless Broadway display"
