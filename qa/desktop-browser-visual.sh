#!/usr/bin/env bash
# Exercises the production bundle in a real browser engine. The native smoke
# script separately launches the Tauri/WebKit binary when a display exists.
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
desktop_root="$repository_root/desktop"
for command in firefox geckodriver curl jq base64 python3; do
  if ! command -v "$command" >/dev/null; then
    echo "production-bundle visual QA requires $command" >&2
    exit 1
  fi
done
if ! python3 -c 'import PIL' >/dev/null 2>&1; then
  echo "production-bundle visual QA requires the Python Pillow package" >&2
  exit 1
fi

temporary_directory=$(mktemp -d)
preview_pid=""
driver_pid=""
session_id=""
cleanup_visual_qa() {
  if [[ -n "$session_id" && "$session_id" != "null" ]]; then
    curl --silent --request DELETE "http://127.0.0.1:4444/session/$session_id" >/dev/null || true
  fi
  for process_id in "$driver_pid" "$preview_pid"; do
    if [[ -n "$process_id" ]] && kill -0 "$process_id" 2>/dev/null; then
      kill "$process_id" 2>/dev/null || true
      wait "$process_id" 2>/dev/null || true
    fi
  done
  rm -r -- "$temporary_directory"
}
trap cleanup_visual_qa EXIT

(
  cd "$desktop_root"
  ./node_modules/.bin/vite preview --host 127.0.0.1 --port 4173
) >"$temporary_directory/vite.log" 2>&1 &
preview_pid=$!
geckodriver --port 4444 >"$temporary_directory/geckodriver.log" 2>&1 &
driver_pid=$!

for _ in {1..50}; do
  if curl --fail --silent http://127.0.0.1:4444/status >/dev/null \
    && curl --fail --silent http://127.0.0.1:4173/ >/dev/null; then
    break
  fi
  sleep 0.2
done
curl --fail --silent http://127.0.0.1:4444/status >/dev/null
curl --fail --silent http://127.0.0.1:4173/ >/dev/null

session_response=$(curl --fail --silent --request POST \
  --header 'Content-Type: application/json' \
  --data '{"capabilities":{"alwaysMatch":{"moz:firefoxOptions":{"args":["-headless"]}}}}' \
  http://127.0.0.1:4444/session)
session_id=$(jq -er '.value.sessionId' <<<"$session_response")
webdriver="http://127.0.0.1:4444/session/$session_id"
curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"width":1440,"height":940}' "$webdriver/window/rect" >/dev/null
curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"url":"http://127.0.0.1:4173/"}' "$webdriver/url" >/dev/null
for _ in {1..30}; do
  chat_ready=$(curl --fail --silent --request POST \
    --header 'Content-Type: application/json' \
    --data '{"script":"return document.querySelector(\".chat-view-header h1\")?.textContent === \"Release readiness\";","args":[]}' \
    "$webdriver/execute/sync" | jq -r '.value')
  [[ "$chat_ready" == "true" ]] && break
  sleep 0.2
done
[[ "$chat_ready" == "true" ]]

chat_geometry=$(curl --fail --silent --request POST \
  --header 'Content-Type: application/json' \
  --data '{"script":"const rect=s=>document.querySelector(s)?.getBoundingClientRect(); return {title:document.querySelector(\".chat-view-header h1\")?.textContent,navigation:Math.round(rect(\".navigation-pane\").width),header:Math.round(rect(\".chat-view-header\").height),inspector:Math.round(rect(\".evidence-inspector\").width),composer:Math.round(rect(\".composer-shell\").height),horizontal:document.documentElement.scrollWidth<=window.innerWidth,appearanceReady:document.documentElement.dataset.appearanceReady===\"true\",stalePlaceholder:document.body.innerText.includes(\"Milestone 01\")};","args":[]}' \
  "$webdriver/execute/sync")
jq -e '.value.title == "Release readiness" and .value.navigation == 208 and .value.header == 48 and (.value.inspector >= 319 and .value.inspector <= 321) and (.value.composer >= 76 and .value.composer <= 200) and .value.horizontal and .value.appearanceReady and (.value.stalePlaceholder | not)' <<<"$chat_geometry" >/dev/null

curl --fail --silent --request POST \
  --header 'Content-Type: application/json' \
  --data '{"script":"const splitter=document.querySelector(\"[aria-label=\\\"Resize navigation pane\\\"]\"); splitter.focus(); splitter.dispatchEvent(new KeyboardEvent(\"keydown\",{key:\"ArrowRight\",bubbles:true})); return true;","args":[]}' \
  "$webdriver/execute/sync" >/dev/null
for _ in {1..20}; do
  splitter_geometry=$(curl --fail --silent --request POST \
    --header 'Content-Type: application/json' \
    --data '{"script":"const splitter=document.querySelector(\"[aria-label=\\\"Resize navigation pane\\\"]\"); return {changed:splitter.getAttribute(\"aria-valuenow\"),focused:document.activeElement===splitter};","args":[]}' \
    "$webdriver/execute/sync")
  [[ $(jq -r '.value.changed' <<<"$splitter_geometry") == "216" ]] && break
  sleep 0.05
done
jq -e '.value.changed == "216" and .value.focused' <<<"$splitter_geometry" >/dev/null
curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"script":"document.activeElement.dispatchEvent(new KeyboardEvent(\"keydown\",{key:\"ArrowLeft\",bubbles:true})); return true;","args":[]}' \
  "$webdriver/execute/sync" >/dev/null
for _ in {1..20}; do
  splitter_reset=$(curl --fail --silent --request POST \
    --header 'Content-Type: application/json' \
    --data '{"script":"return document.querySelector(\"[aria-label=\\\"Resize navigation pane\\\"]\").getAttribute(\"aria-valuenow\");","args":[]}' \
    "$webdriver/execute/sync" | jq -r '.value')
  [[ "$splitter_reset" == "208" ]] && break
  sleep 0.05
done
[[ "$splitter_reset" == "208" ]]

curl --fail --silent "$webdriver/screenshot" \
  | jq -er '.value' \
  | base64 --decode >"$temporary_directory/chat.png"
curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"script":"[...document.querySelectorAll(\"button\")].find(button=>button.textContent.includes(\"Workflows\"))?.click(); return true;","args":[]}' \
  "$webdriver/execute/sync" >/dev/null
for _ in {1..30}; do
  workflow_ready=$(curl --fail --silent --request POST \
    --header 'Content-Type: application/json' \
    --data '{"script":"return document.querySelector(\".workflow-editor h1\")?.textContent === \"Repository Engineer\";","args":[]}' \
    "$webdriver/execute/sync" | jq -r '.value')
  [[ "$workflow_ready" == "true" ]] && break
  sleep 0.2
done
[[ "$workflow_ready" == "true" ]]
workflow_geometry=$(curl --fail --silent --request POST \
  --header 'Content-Type: application/json' \
  --data '{"script":"const run=[...document.querySelectorAll(\"button\")].find(button=>button.textContent.trim()===\"Run\"); return {title:document.querySelector(\".workflow-editor h1\")?.textContent,missing:document.querySelector(\".dependency-banner\")?.textContent.includes(\"plugin.code-review@2.x\"),nodes:document.querySelectorAll(\".react-flow__node\").length,runDisabled:run?.disabled,property:document.querySelector(\".properties-pane h2\")?.textContent,horizontal:document.documentElement.scrollWidth<=window.innerWidth};","args":[]}' \
  "$webdriver/execute/sync")
jq -e '.value.title == "Repository Engineer" and .value.missing and .value.nodes == 6 and .value.runDisabled and .value.property == "acme.code-review@2.x" and .value.horizontal' <<<"$workflow_geometry" >/dev/null
curl --fail --silent --request POST \
  --header 'Content-Type: application/json' \
  --data '{"script":"const source=[...document.querySelectorAll(\"button\")].find(button=>button.getAttribute(\"aria-label\")===\"Add a Model node to the canvas\"); const target=document.querySelector(\".react-flow\"); const transfer=new DataTransfer(); source.dispatchEvent(new DragEvent(\"dragstart\",{bubbles:true,dataTransfer:transfer})); const rect=target.getBoundingClientRect(); target.dispatchEvent(new DragEvent(\"dragover\",{bubbles:true,cancelable:true,dataTransfer:transfer,clientX:rect.left+rect.width/2,clientY:rect.top+rect.height/2})); target.dispatchEvent(new DragEvent(\"drop\",{bubbles:true,cancelable:true,dataTransfer:transfer,clientX:rect.left+rect.width/2,clientY:rect.top+rect.height/2})); return document.querySelectorAll(\".react-flow__node\").length;","args":[]}' \
  "$webdriver/execute/sync" >/dev/null
for _ in {1..20}; do
  drag_drop_nodes=$(curl --fail --silent --request POST \
    --header 'Content-Type: application/json' \
    --data '{"script":"return document.querySelectorAll(\".react-flow__node\").length;","args":[]}' \
    "$webdriver/execute/sync" | jq -r '.value')
  [[ "$drag_drop_nodes" == "7" ]] && break
  sleep 0.05
done
[[ "$drag_drop_nodes" == "7" ]]
curl --fail --silent "$webdriver/screenshot" \
  | jq -er '.value' \
  | base64 --decode >"$temporary_directory/workflow.png"

curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"script":"[...document.querySelectorAll(\"button\")].find(button=>button.textContent.includes(\"Settings\"))?.click(); return true;","args":[]}' \
  "$webdriver/execute/sync" >/dev/null
for _ in {1..30}; do
  settings_ready=$(curl --fail --silent --request POST \
    --header 'Content-Type: application/json' \
    --data '{"script":"return document.querySelector(\".settings-workspace h1\")?.textContent === \"Settings\";","args":[]}' \
    "$webdriver/execute/sync" | jq -r '.value')
  [[ "$settings_ready" == "true" ]] && break
  sleep 0.2
done
[[ "$settings_ready" == "true" ]]
curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"script":"[...document.querySelectorAll(\"button\")].find(button=>button.textContent.trim()===\"Appearance\")?.click(); return true;","args":[]}' \
  "$webdriver/execute/sync" >/dev/null
for _ in {1..30}; do
  appearance_ready=$(curl --fail --silent --request POST \
    --header 'Content-Type: application/json' \
    --data '{"script":"return document.querySelectorAll(\".appearance-options label\").length === 3;","args":[]}' \
    "$webdriver/execute/sync" | jq -r '.value')
  [[ "$appearance_ready" == "true" ]] && break
  sleep 0.2
done
[[ "$appearance_ready" == "true" ]]
dark_surface=$(curl --fail --silent --request POST \
  --header 'Content-Type: application/json' \
  --data '{"script":"const label=[...document.querySelectorAll(\".appearance-options label\")].find(item=>item.textContent.includes(\"Dark\")); label?.querySelector(\"input\")?.click(); return getComputedStyle(document.documentElement).getPropertyValue(\"--aw-window\").trim().toLowerCase();","args":[]}' \
  "$webdriver/execute/sync" | jq -r '.value')
[[ "$dark_surface" == "#111318" ]]

curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"width":760,"height":700}' "$webdriver/window/rect" >/dev/null
narrow_geometry=$(curl --fail --silent --request POST \
  --header 'Content-Type: application/json' \
  --data '{"script":"return {navigation:Math.round(document.querySelector(\".navigation-pane\").getBoundingClientRect().width),horizontal:document.documentElement.scrollWidth<=window.innerWidth};","args":[]}' \
  "$webdriver/execute/sync")
jq -e '.value.navigation == 44 and .value.horizontal' <<<"$narrow_geometry" >/dev/null

# A second Firefox profile uses a true 2x device-pixel scale. A 720x470 CSS
# window therefore exercises a 1440x940 physical surface at 200% scale.
curl --fail --silent --request DELETE "$webdriver" >/dev/null
session_id=""
scaled_session_response=$(curl --fail --silent --request POST \
  --header 'Content-Type: application/json' \
  --data '{"capabilities":{"alwaysMatch":{"moz:firefoxOptions":{"args":["-headless"],"prefs":{"layout.css.devPixelsPerPx":"2.0"}}}}}' \
  http://127.0.0.1:4444/session)
session_id=$(jq -er '.value.sessionId' <<<"$scaled_session_response")
webdriver="http://127.0.0.1:4444/session/$session_id"
curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"width":720,"height":470}' "$webdriver/window/rect" >/dev/null
curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"url":"http://127.0.0.1:4173/"}' "$webdriver/url" >/dev/null
for _ in {1..30}; do
  scaled_ready=$(curl --fail --silent --request POST \
    --header 'Content-Type: application/json' \
    --data '{"script":"return document.querySelector(\".chat-view-header h1\")?.textContent === \"Release readiness\";","args":[]}' \
    "$webdriver/execute/sync" | jq -r '.value')
  [[ "$scaled_ready" == "true" ]] && break
  sleep 0.2
done
[[ "$scaled_ready" == "true" ]]
zoom_geometry=$(curl --fail --silent --request POST \
  --header 'Content-Type: application/json' \
  --data '{"script":"return {scale:window.devicePixelRatio,width:window.innerWidth,navigation:Math.round(document.querySelector(\".navigation-pane\").getBoundingClientRect().width),horizontal:document.documentElement.scrollWidth<=window.innerWidth};","args":[]}' \
  "$webdriver/execute/sync")
jq -e '.value.scale >= 1.9 and .value.width <= 800 and .value.navigation == 44 and .value.horizontal' <<<"$zoom_geometry" >/dev/null

python3 - "$temporary_directory/chat.png" "$temporary_directory/workflow.png" <<'PY'
from collections import Counter
from pathlib import Path
import sys
from PIL import Image

for raw in sys.argv[1:]:
    path = Path(raw)
    image = Image.open(path).convert("RGB").resize((320, 200))
    colors = Counter(image.getdata())
    dominant = colors.most_common(1)[0][1] / (image.width * image.height)
    if len(colors) < 32 or dominant >= 0.82:
        raise SystemExit(f"{path.name} appears blank: colors={len(colors)}, dominant={dominant:.1%}")
    print(f"{path.name}: colors={len(colors)}, dominant={dominant:.1%}")
PY

echo "production bundle matches compact Chat/workflow geometry, drag/drop, dark tokens, 200% scale, and narrow-window behavior"
