#!/usr/bin/env bash
# Drives the actual Tauri/WebKit application through Broadway, including IPC,
# provider HTTP effects, durable state, process restart, and visible follow-up.
set -Eeuo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
desktop_root="$repository_root/desktop"
binary_path="$desktop_root/src-tauri/target/debug/aworkit-desktop"
fixture_script="$repository_root/qa/fixtures/openai-compatible-fixture.mjs"

for command in broadwayd firefox geckodriver curl jq base64 node sqlite3 python3; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "native Simple Chat E2E requires $command" >&2
    exit 1
  fi
done
if ! python3 -c 'import PIL' >/dev/null 2>&1; then
  echo "native Simple Chat E2E requires the Python Pillow package" >&2
  exit 1
fi

if [[ "${AWORKIT_SKIP_NATIVE_BUILD:-0}" != "1" ]]; then
  (
    cd "$desktop_root"
    ./node_modules/.bin/tsc --noEmit
    ./node_modules/.bin/vite build
    ./node_modules/.bin/tauri build --debug --no-bundle --ci \
      --config '{"build":{"beforeBuildCommand":""}}'
  )
fi
test -x "$binary_path"

temporary_directory=$(mktemp -d)
fixture_pid=""
broadway_pid=""
driver_pid=""
application_pid=""
webdriver_session_id=""

report_failure() {
  local status=$?
  trap - ERR
  echo "native Simple Chat E2E failed (exit $status)" >&2
  if [[ -n "$webdriver_session_id" && -n "${webdriver:-}" ]]; then
    failure_screenshot="/tmp/aworkit-native-simple-chat-e2e-failure.png"
    if curl --fail --silent "$webdriver/screenshot" \
      | jq -er '.value' \
      | base64 --decode >"$failure_screenshot"; then
      echo "failure screenshot: $failure_screenshot" >&2
    fi
  fi
  for diagnostic in app.log fixture.stderr fixture-requests.jsonl broadway.log geckodriver.log; do
    if [[ -s "$temporary_directory/$diagnostic" ]]; then
      echo "--- $diagnostic ---" >&2
      sed -n '1,200p' "$temporary_directory/$diagnostic" >&2
    fi
  done
  exit "$status"
}

cleanup() {
  if [[ -n "$webdriver_session_id" && -n "$driver_pid" ]]; then
    curl --silent --request DELETE \
      "http://127.0.0.1:$driver_port/session/$webdriver_session_id" >/dev/null || true
  fi
  for process_id in "$application_pid" "$driver_pid" "$broadway_pid" "$fixture_pid"; do
    if [[ -n "$process_id" ]] && kill -0 "$process_id" 2>/dev/null; then
      kill "$process_id" 2>/dev/null || true
      wait "$process_id" 2>/dev/null || true
    fi
  done
  rm -r -- "$temporary_directory"
}
trap report_failure ERR
trap cleanup EXIT

ready_file="$temporary_directory/fixture-ready.json"
request_log="$temporary_directory/fixture-requests.jsonl"
model_id="aworkit-native-model"
node "$fixture_script" \
  --ready-file "$ready_file" \
  --request-log "$request_log" \
  --model "$model_id" \
  --allow-unauthenticated true \
  >"$temporary_directory/fixture.stdout" \
  2>"$temporary_directory/fixture.stderr" &
fixture_pid=$!
for _ in {1..100}; do
  [[ -s "$ready_file" ]] && break
  sleep 0.05
done
test -s "$ready_file"
base_url=$(jq -er '.baseUrl' "$ready_file")

broadway_port=$((18000 + ($$ % 18000)))
driver_port=$((broadway_port + 1))
broadway_display=$((400 + ($$ % 2000)))
geckodriver --port "$driver_port" >"$temporary_directory/geckodriver.log" 2>&1 &
driver_pid=$!

start_broadway() {
  broadwayd --address=127.0.0.1 --port="$broadway_port" \
    ":$broadway_display" >>"$temporary_directory/broadway.log" 2>&1 &
  broadway_pid=$!
}

start_broadway
for _ in {1..60}; do
  if curl --fail --silent "http://127.0.0.1:$broadway_port/" >/dev/null \
    && curl --fail --silent "http://127.0.0.1:$driver_port/status" >/dev/null; then
    break
  fi
  sleep 0.1
done
curl --fail --silent "http://127.0.0.1:$broadway_port/" >/dev/null
curl --fail --silent "http://127.0.0.1:$driver_port/status" >/dev/null

start_application() {
  GDK_BACKEND=broadway \
    BROADWAY_DISPLAY=":$broadway_display" \
    XDG_DATA_HOME="$temporary_directory/xdg-data" \
    XDG_CONFIG_HOME="$temporary_directory/xdg-config" \
    WEBKIT_DISABLE_COMPOSITING_MODE=1 \
    "$binary_path" >>"$temporary_directory/app.log" 2>&1 &
  application_pid=$!
}

start_application
session_response=$(curl --fail --silent --request POST \
  --header 'Content-Type: application/json' \
  --data '{"capabilities":{"alwaysMatch":{"moz:firefoxOptions":{"args":["-headless"]}}}}' \
  "http://127.0.0.1:$driver_port/session")
webdriver_session_id=$(jq -er '.value.sessionId' <<<"$session_response")
webdriver="http://127.0.0.1:$driver_port/session/$webdriver_session_id"
curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"width":1600,"height":1250}' "$webdriver/window/rect" >/dev/null
curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data "{\"url\":\"http://127.0.0.1:$broadway_port/\"}" "$webdriver/url" >/dev/null

wait_for_canvas() {
  local ready=false
  for _ in {1..80}; do
    ready=$(curl --fail --silent --request POST \
      --header 'Content-Type: application/json' \
      --data '{"script":"const canvas=document.querySelector(\"canvas\"); return canvas!==null && canvas.width>=1400 && canvas.height>=1000;","args":[]}' \
      "$webdriver/execute/sync" | jq -r '.value')
    [[ "$ready" == "true" ]] && break
    sleep 0.1
  done
  [[ "$ready" == "true" ]]
  kill -0 "$application_pid"
}

release_actions() {
  curl --fail --silent --request DELETE "$webdriver/actions" >/dev/null
}

click_at() {
  local x=$1
  local y=$2
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data "{\"actions\":[{\"type\":\"pointer\",\"id\":\"mouse\",\"parameters\":{\"pointerType\":\"mouse\"},\"actions\":[{\"type\":\"pointerMove\",\"duration\":0,\"origin\":\"viewport\",\"x\":$x,\"y\":$y},{\"type\":\"pointerDown\",\"button\":0},{\"type\":\"pointerUp\",\"button\":0}]}]}" \
    "$webdriver/actions" >/dev/null
  release_actions
  # Pointer delivery is asynchronous across Firefox → Broadway → GTK →
  # WebKit. Do not let the first following key race the focus transition.
  sleep 0.2
}

type_text() {
  local value=$1
  local payload
  payload=$(jq -nc --arg value "$value" \
    '{actions:[{type:"key",id:"keyboard",actions:($value|explode|map(([.]|implode) as $character|[{type:"keyDown",value:$character},{type:"pause",duration:12},{type:"keyUp",value:$character},{type:"pause",duration:12}])|add)}]}')
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data "$payload" "$webdriver/actions" >/dev/null
  release_actions
  # Broadway/WebKit consumes native key events asynchronously. Let its queue
  # drain before a following pointer event can move focus to another field.
  sleep 0.35
}

press_key() {
  local value=$1
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data "{\"actions\":[{\"type\":\"key\",\"id\":\"keyboard\",\"actions\":[{\"type\":\"keyDown\",\"value\":\"$value\"},{\"type\":\"keyUp\",\"value\":\"$value\"}]}]}" \
    "$webdriver/actions" >/dev/null
  release_actions
}

select_all() {
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data '{"actions":[{"type":"key","id":"keyboard","actions":[{"type":"keyDown","value":"\uE009"},{"type":"keyDown","value":"a"},{"type":"keyUp","value":"a"},{"type":"keyUp","value":"\uE009"}]}]}' \
    "$webdriver/actions" >/dev/null
  release_actions
}

scroll_at() {
  local x=$1
  local y=$2
  local delta_y=$3
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data "{\"actions\":[{\"type\":\"wheel\",\"id\":\"wheel\",\"actions\":[{\"type\":\"scroll\",\"x\":$x,\"y\":$y,\"deltaX\":0,\"deltaY\":$delta_y,\"duration\":200,\"origin\":\"viewport\"}]}]}" \
    "$webdriver/actions" >/dev/null
  release_actions
  sleep 0.3
}

open_route() {
  local key=$1
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data "{\"actions\":[{\"type\":\"key\",\"id\":\"keyboard\",\"actions\":[{\"type\":\"keyDown\",\"value\":\"\\uE009\"},{\"type\":\"keyDown\",\"value\":\"$key\"},{\"type\":\"keyUp\",\"value\":\"$key\"},{\"type\":\"keyUp\",\"value\":\"\\uE009\"}]}]}" \
    "$webdriver/actions" >/dev/null
  release_actions
  sleep 0.7
}

request_count() {
  local kind=$1
  jq -s --arg kind "$kind" '[.[] | select(.kind == $kind)] | length' "$request_log"
}

wait_for_request_count() {
  local kind=$1
  local expected=$2
  local actual=0
  for _ in {1..100}; do
    actual=$(request_count "$kind")
    [[ "$actual" -ge "$expected" ]] && break
    sleep 0.1
  done
  [[ "$actual" -eq "$expected" ]]
}

wait_for_canvas
click_at 800 500
open_route ','

# Configure Settings v2 through the literal Tauri/WebKit surface. Coordinates
# address the fixed 1440x940 application window inside the isolated 1600x1250
# Broadway surface; every action is a real GTK pointer, keyboard, or wheel event.
click_at 784 372
click_at 1120 484
select_all
type_text "$base_url"
click_at 1393 838
wait_for_request_count models 1
click_at 1199 552
scroll_at 1200 850 420
click_at 851 808
click_at 1429 580
wait_for_request_count models 2
click_at 400 325
scroll_at 1200 850 -2000
click_at 1300 895
click_at 1150 946
click_at 1485 208

settings_root="$temporary_directory/xdg-data/com.aworkit.desktop/runtime/documents/configuration"
settings_body=""
for _ in {1..100}; do
  if [[ -s "$settings_root/manifest.json" ]]; then
    relative_path=$(jq -r '.documents["settings.desktop"].relative_path // empty' \
      "$settings_root/manifest.json")
    if [[ -n "$relative_path" && -s "$settings_root/$relative_path" ]]; then
      settings_body="$settings_root/$relative_path"
      if jq -e --arg base "$base_url" --arg model "$model_id" \
        '.schemaVersion == 2
          and (.providers | length == 1)
          and .providers[0].baseUrl == $base
          and .providers[0].models[0].remoteId == $model
          and .providers[0].enabled
          and .providers[0].models[0].enabled
          and ([.modelTiers[] | select(.id == "tier:balanced" and .resolution.strategy == "exact")] | length == 1)' \
        "$settings_body" >/dev/null; then
        break
      fi
    fi
  fi
  sleep 0.1
done
test -n "$settings_body"
provider_id=$(jq -er '.providers[0].id' "$settings_body")
model_local_id=$(jq -er '.providers[0].models[0].id' "$settings_body")
jq -e \
  --arg base "$base_url" \
  --arg model "$model_id" \
  --arg provider_id "$provider_id" \
  --arg model_local_id "$model_local_id" '
    .schemaVersion == 2
    and (has("provider") | not)
    and (.providers | length == 1)
    and .providers[0].kind == "openai_compatible"
    and .providers[0].baseUrl == $base
    and .providers[0].enabled
    and (.providers[0].credentialRef == null)
    and (.providers[0].models | length == 1)
    and .providers[0].models[0].remoteId == $model
    and .providers[0].models[0].enabled
    and ([.providers[0].models[0].capabilities[]] | index("text") != null)
    and ([.modelTiers[] | select(
      .id == "tier:balanced"
      and .resolution.strategy == "exact"
      and .resolution.target.providerId == $provider_id
      and .resolution.target.modelId == $model_local_id
    )] | length == 1)
    and ([.tools[].id] | sort == [
      "tool.files.edit", "tool.files.read", "tool.files.search",
      "tool.python.host", "tool.shell.host"
    ])
    and ([.tools[].enabled] | all(. == false))
    and .credentials == []
    and .extensions == []
    and .mcpServers == []
    and .externalAgents == []
    and .projects == []
  ' "$settings_body" >/dev/null
fixture_secret=$(jq -er '.apiKey' "$ready_file")
if grep -F -- "$fixture_secret" "$settings_body" >/dev/null; then
  echo "canonical Settings v2 leaked the fixture credential" >&2
  exit 1
fi
settings_version=$(jq -er '.documents["settings.desktop"].document_version' \
  "$settings_root/manifest.json")

workflow_root="$temporary_directory/xdg-data/com.aworkit.desktop/runtime/documents/workflows"
workflow_relative_path=$(jq -er '.documents["workflow.simple-chat"].relative_path' \
  "$workflow_root/manifest.json")
workflow_body="$workflow_root/$workflow_relative_path"
test -s "$workflow_body"
jq -e '
  .schemaVersion == 1
  and .id == "workflow.simple-chat"
  and ([.nodes[].type] == ["input", "agent", "output", "wait"])
  and ([.edges[] | [.source, .target]] == [
    ["input.1", "agent.1"],
    ["agent.1", "output.1"],
    ["output.1", "wait.1"]
  ])
  and .nodes[1].configuration.modelTierId == "tier:balanced"
  and .nodes[1].configuration.toolIds == []
' "$workflow_body" >/dev/null
workflow_version=$(jq -er '.documents["workflow.simple-chat"].document_version' \
  "$workflow_root/manifest.json")

sleep 0.8
configured_screenshot="/tmp/aworkit-native-settings-v2.png"
curl --fail --silent "$webdriver/screenshot" \
  | jq -er '.value' \
  | base64 --decode >"$configured_screenshot"

open_route '1'
click_at 760 1038
type_text 'native hello'
press_key '\uE007'
wait_for_request_count chat.completion 1

history_database="$temporary_directory/xdg-data/com.aworkit.desktop/runtime/history/aworkit.sqlite3"
for _ in {1..100}; do
  first_reply=$(sqlite3 "$history_database" \
    "SELECT json_extract(payload, '$.body') FROM semantic_events WHERE kind='message.assistant' ORDER BY sequence DESC LIMIT 1;" 2>/dev/null || true)
  [[ "$first_reply" == 'AWORKIT_FIXTURE_REPLY_1: native hello' ]] && break
  sleep 0.1
done
[[ "$first_reply" == 'AWORKIT_FIXTURE_REPLY_1: native hello' ]]

first_chat_id=$(sqlite3 "$history_database" \
  "SELECT json_extract(payload, '$.chatId') FROM semantic_events WHERE kind='chat.started' ORDER BY sequence DESC LIMIT 1;")
first_run_id=$(sqlite3 "$history_database" \
  "SELECT json_extract(payload, '$.runId') FROM semantic_events WHERE kind='chat.started' ORDER BY sequence DESC LIMIT 1;")
[[ "$first_chat_id" =~ ^chat\.[0-9a-f]{40}$ ]]
[[ "$first_run_id" =~ ^run\.[0-9a-f]{40}$ ]]
[[ "$first_chat_id" != "chat.local" ]]
[[ "$first_run_id" != "run.local" ]]

frozen_record=$(sqlite3 "$history_database" \
  "SELECT json_extract(payload, '$.record') FROM semantic_events WHERE chat_id='chat.frozen-sessions' AND kind='chat.execution-context-frozen' ORDER BY sequence DESC LIMIT 1;")
jq -e \
  --arg chat "$first_chat_id" \
  --arg run "$first_run_id" \
  --arg base "$base_url" \
  --arg provider_id "$provider_id" \
  --arg model_id "$model_local_id" \
  --arg remote_model "$model_id" '
    .context.schemaVersion == 1
    and .context.identity.chatId == $chat
    and .context.identity.runId == $run
    and .context.workflowId == "workflow.simple-chat"
    and .context.workflowSnapshot.nodes[1].type == "agent"
    and .context.modelTierId == "tier:balanced"
    and .context.providerId == $provider_id
    and .context.providerKind == "openai_compatible"
    and .context.providerBaseUrl == $base
    and .context.modelId == $model_id
    and .context.remoteModelId == $remote_model
    and (.context.opaqueBinding == null)
    and (.contextHash | test("^sha256:[0-9a-f]{64}$"))
    and (.context.workflowSnapshotHash | test("^sha256:[0-9a-f]{64}$"))
    and (.context.providerHash | test("^sha256:[0-9a-f]{64}$"))
    and (.context.modelHash | test("^sha256:[0-9a-f]{64}$"))
  ' <<<"$frozen_record" >/dev/null
frozen_context_hash=$(jq -er '.contextHash' <<<"$frozen_record")
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE chat_id='chat.frozen-sessions' AND kind='chat.execution-context-frozen';") -eq 1 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE kind='message.assistant' AND json_extract(payload, '$.frozenContextHash')='$frozen_context_hash';") -eq 1 ]]

sleep 1
first_turn_screenshot="/tmp/aworkit-native-simple-chat-first-turn.png"
curl --fail --silent "$webdriver/screenshot" \
  | jq -er '.value' \
  | base64 --decode >"$first_turn_screenshot"

# Hard process boundary: the second application instance gets only the same
# XDG profile. No settings command or provider test is repeated.
kill "$application_pid"
wait "$application_pid" 2>/dev/null || true
application_pid=""

# Ubuntu's Broadway daemon can assert while a GTK client disconnects. Restart
# only that stateless display transport; the application receives solely the
# original XDG profile, which is the durable-state boundary under test.
for _ in {1..50}; do
  ! kill -0 "$broadway_pid" 2>/dev/null && break
  sleep 0.02
done
if kill -0 "$broadway_pid" 2>/dev/null; then
  kill "$broadway_pid" 2>/dev/null || true
fi
wait "$broadway_pid" 2>/dev/null || true
broadway_pid=""
start_broadway
for _ in {1..60}; do
  curl --fail --silent "http://127.0.0.1:$broadway_port/" >/dev/null && break
  sleep 0.1
done
curl --fail --silent "http://127.0.0.1:$broadway_port/" >/dev/null
start_application
curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data "{\"url\":\"http://127.0.0.1:$broadway_port/?restart=1\"}" \
  "$webdriver/url" >/dev/null
sleep 1.5
wait_for_canvas
open_route '1'
click_at 760 1038
type_text 'native again'
press_key '\uE007'
wait_for_request_count chat.completion 2

second_reply=""
for _ in {1..100}; do
  second_reply=$(sqlite3 "$history_database" \
    "SELECT json_extract(payload, '$.body') FROM semantic_events WHERE kind='message.assistant' ORDER BY sequence DESC LIMIT 1;" 2>/dev/null || true)
  [[ "$second_reply" == 'AWORKIT_FIXTURE_REPLY_2: native again' ]] && break
  sleep 0.1
done
[[ "$second_reply" == 'AWORKIT_FIXTURE_REPLY_2: native again' ]]
[[ $(request_count models) -eq 2 ]]
[[ $(request_count chat.completion) -eq 2 ]]
[[ $(jq -er '.documents["settings.desktop"].document_version' \
  "$settings_root/manifest.json") -eq "$settings_version" ]]
[[ $(jq -er '.documents["workflow.simple-chat"].document_version' \
  "$workflow_root/manifest.json") -eq "$workflow_version" ]]
jq -s -e '
  [.[] | select(.kind == "chat.completion")] as $requests
  | ($requests | length) == 2
    and $requests[0].model == "aworkit-native-model"
    and $requests[0].stream == false
    and ($requests[0].messages == [
      {"role":"user","content":"native hello"}
    ])
    and ($requests[1].messages == [
      {"role":"user","content":"native hello"},
      {"role":"assistant","content":"AWORKIT_FIXTURE_REPLY_1: native hello"},
      {"role":"user","content":"native again"}
    ])
' "$request_log" >/dev/null

[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE kind='chat.started' AND json_extract(payload, '$.chatId')='$first_chat_id' AND json_extract(payload, '$.runId')='$first_run_id';") -eq 1 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE kind='message.user';") -eq 2 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE kind='message.assistant';") -eq 2 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE kind='execution.failed';") -eq 0 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE chat_id='chat.frozen-sessions' AND kind='chat.execution-context-frozen' AND json_extract(payload, '$.record.contextHash')='$frozen_context_hash';") -eq 1 ]]
mapfile -t assistant_history < <(sqlite3 "$history_database" \
  "SELECT json_extract(payload, '$.body') FROM semantic_events WHERE kind='message.assistant' ORDER BY sequence;")
[[ "${assistant_history[0]}" == 'AWORKIT_FIXTURE_REPLY_1: native hello' ]]
[[ "${assistant_history[1]}" == 'AWORKIT_FIXTURE_REPLY_2: native again' ]]
if grep -F -- "$fixture_secret" "$settings_body" "$workflow_body" >/dev/null; then
  echo "credential value appeared in a canonical JSON document after restart" >&2
  exit 1
fi

# The SQLite commit precedes the Tauri snapshot event reaching WebKit. Keep
# the process alive long enough for the visible projection to settle before
# capturing the native acceptance frame.
sleep 1.5
final_screenshot="/tmp/aworkit-native-simple-chat-after-restart.png"
curl --fail --silent "$webdriver/screenshot" \
  | jq -er '.value' \
  | base64 --decode >"$final_screenshot"
python3 - "$configured_screenshot" "$first_turn_screenshot" "$final_screenshot" <<'PY'
from collections import Counter
from pathlib import Path
import sys
from PIL import Image

for raw in sys.argv[1:]:
    path = Path(raw)
    image = Image.open(path).convert("RGB")
    sample = image.crop(
        (120, 120, min(image.width, 1570), min(image.height, 1110))
    ).resize((320, 200))
    colors = Counter(sample.getdata())
    dominant = colors.most_common(1)[0][1] / (sample.width * sample.height)
    if image.width < 1500 or image.height < 1100 or len(colors) < 700 or dominant >= 0.78:
        raise SystemExit(
            f"native screenshot is incomplete: {path.name} {image.width}x{image.height}, "
            f"colors={len(colors)}, dominant={dominant:.1%}"
        )
    print(
        f"{path}: {image.width}x{image.height}; "
        f"colors={len(colors)}; dominant={dominant:.1%}"
    )
PY

echo "native Tauri acceptance passed: Settings v2 discovery/test/save, exact Simple Chat, hard restart, frozen identity/context, persisted two-turn assistant history"
