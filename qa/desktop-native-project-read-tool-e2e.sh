#!/usr/bin/env bash
# Literal Tauri/WebKit acceptance for one saved-project, read-tool Agent loop.
set -Eeuo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
desktop_root="$repository_root/desktop"
native_binary_path="$desktop_root/src-tauri/target/debug/aworkit-desktop"
fixture_script="$repository_root/qa/fixtures/openai-compatible-fixture.mjs"

for command in broadwayd firefox geckodriver curl jq base64 node sqlite3 python3 sha256sum cut grep gdbus rg; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "native project/read-tool E2E requires $command" >&2
    exit 1
  fi
done
if ! python3 -c 'import PIL' >/dev/null 2>&1; then
  echo "native project/read-tool E2E requires the Python Pillow package" >&2
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
test -x "$native_binary_path"

native_temporary_directory=$(mktemp -d)
project_root=$(mktemp -d "$repository_root/aworkit-native-project.XXXXXX")
fixture_marker="AWORKIT_PROJECT_FILE_CONTENT_7d0a2f"
tool_prompt="Read notes.txt from the selected project."
followup_prompt="Confirm after restart without reading the file again."
printf '%s\n' "$fixture_marker" >"$project_root/notes.txt"
printf '%s\n' "OUTSIDE_PROJECT_MUST_NOT_BE_READ" \
  >"$native_temporary_directory/outside.txt"
project_content_hash_before=$(sha256sum "$project_root/notes.txt" | cut -d' ' -f1)
outside_content_hash_before=$(sha256sum "$native_temporary_directory/outside.txt" | cut -d' ' -f1)

native_broadway_port=$((18000 + ($$ % 18000)))
native_driver_port=$((native_broadway_port + 1))
native_broadway_display=$((400 + ($$ % 2000)))
# shellcheck source=qa/lib/desktop-native-broadway.sh
source "$repository_root/qa/lib/desktop-native-broadway.sh"

fixture_pid=""
credential_ref=""
ready_file="$native_temporary_directory/fixture-ready.json"
request_log="$native_temporary_directory/fixture-requests.jsonl"
model_id="aworkit-tool-model"

report_failure() {
  local status=$?
  trap - ERR
  set +e
  echo "native project/read-tool E2E failed (exit $status)" >&2
  if [[ -n "$native_webdriver_session_id" && -n "$native_webdriver" ]]; then
    failure_screenshot="/tmp/aworkit-native-project-read-tool-e2e-failure.png"
    if native_capture_screenshot "$failure_screenshot"; then
      echo "failure screenshot: $failure_screenshot" >&2
    fi
  fi
  local diagnostic
  for diagnostic in app.log fixture.stderr fixture-requests.jsonl broadway.log geckodriver.log; do
    if [[ -s "$native_temporary_directory/$diagnostic" ]]; then
      echo "--- $diagnostic ---" >&2
      sed -n '1,260p' "$native_temporary_directory/$diagnostic" >&2
    fi
  done
  exit "$status"
}

cleanup() {
  set +e
  native_stop_processes
  if [[ -n "$credential_ref" ]]; then
    credential_search=$(gdbus call --session \
      --dest org.freedesktop.secrets \
      --object-path /org/freedesktop/secrets \
      --method org.freedesktop.Secret.Service.SearchItems \
      "{'service': 'org.aworkit.credentials.v1', 'username': '$credential_ref'}" \
      2>/dev/null || true)
    while IFS= read -r credential_item_path; do
      [[ "$credential_item_path" == /org/freedesktop/secrets/collection/* ]] \
        || continue
      gdbus call --session \
        --dest org.freedesktop.secrets \
        --object-path "$credential_item_path" \
        --method org.freedesktop.Secret.Item.Delete >/dev/null 2>&1 || true
    done < <(rg -o "/org/freedesktop/secrets/collection/[^']+" \
      <<<"$credential_search")
  fi
  if [[ -n "$fixture_pid" ]] && kill -0 "$fixture_pid" 2>/dev/null; then
    kill "$fixture_pid" 2>/dev/null || true
    wait "$fixture_pid" 2>/dev/null || true
  fi
  rm -r -- "$native_temporary_directory"
  if [[ "$project_root" == "$repository_root"/aworkit-native-project.* ]]; then
    rm -r -- "$project_root"
  else
    echo "refusing to remove unexpected project fixture path: $project_root" >&2
  fi
}
trap report_failure ERR
trap cleanup EXIT

node "$fixture_script" \
  --ready-file "$ready_file" \
  --request-log "$request_log" \
  --model "$model_id" \
  --allow-unauthenticated false \
  --tool-call-mode read-project-file \
  --tool-path notes.txt \
  --tool-prompt "$tool_prompt" \
  --followup-prompt "$followup_prompt" \
  --expected-tool-content "$fixture_marker" \
  >"$native_temporary_directory/fixture.stdout" \
  2>"$native_temporary_directory/fixture.stderr" &
fixture_pid=$!
for _ in {1..100}; do
  [[ -s "$ready_file" ]] && break
  sleep 0.05
done
test -s "$ready_file"
base_url=$(jq -er '.baseUrl' "$ready_file")
fixture_secret=$(jq -er '.apiKey' "$ready_file")

request_count() {
  local kind=$1
  jq -s --arg kind "$kind" '[.[] | select(.kind == $kind)] | length' \
    "$request_log"
}

wait_for_request_count() {
  local kind=$1
  local expected=$2
  local actual=0
  for _ in {1..160}; do
    actual=$(request_count "$kind")
    [[ "$actual" -ge "$expected" ]] && break
    sleep 0.1
  done
  [[ "$actual" -eq "$expected" ]]
}

native_start_driver
native_start_broadway
native_wait_for_transports
native_start_application
native_create_webdriver_session
sleep 1.5
native_wait_for_canvas

# Settings v2: save the provider endpoint first so the write-only credential
# can be bound to that exact provider identity and endpoint before any network
# request. Discovery, Test, and Chat must then redeem the stored credential.
native_click_at 200 1038
sleep 0.7
native_click_at 784 372
native_click_at 1120 484
native_select_all
native_type_text "$base_url"
native_click_at 1485 208

settings_root="$native_temporary_directory/xdg-data/com.aworkit.desktop/runtime/documents/configuration"
settings_body=""
for _ in {1..120}; do
  if [[ -s "$settings_root/manifest.json" ]]; then
    relative_path=$(jq -r '.documents["settings.desktop"].relative_path // empty' \
      "$settings_root/manifest.json")
    if [[ -n "$relative_path" && -s "$settings_root/$relative_path" ]]; then
      settings_body="$settings_root/$relative_path"
      jq -e --arg base "$base_url" '
        (.providers | length == 1)
        and .providers[0].baseUrl == $base
        and (.providers[0].enabled | not)
        and (.providers[0].models | length == 0)
        and (.credentials | length == 0)
      ' "$settings_body" >/dev/null && break
    fi
  fi
  sleep 0.1
done
test -n "$settings_body"
provider_id=$(jq -er '.providers[0].id' "$settings_body")

native_click_at 400 392
native_click_at 1490 344
native_click_at 835 476
native_type_text 'Aworkit native fixture'
native_click_at 1000 541
native_click_at 1000 591
native_click_at 1190 638
native_type_text "$fixture_secret"
native_click_at 1465 731

for _ in {1..120}; do
  relative_path=$(jq -r '.documents["settings.desktop"].relative_path // empty' \
    "$settings_root/manifest.json")
  settings_body="$settings_root/$relative_path"
  credential_ref=$(jq -r '.credentials[0].credentialRef // empty' "$settings_body")
  if jq -e \
    --arg provider_id "$provider_id" \
    --arg base "$base_url" '
      (.credentials | length == 1)
      and .credentials[0].label == "Aworkit native fixture"
      and .credentials[0].kind == "api_key"
      and .credentials[0].fieldNames == ["api_key"]
      and .credentials[0].revision == 1
      and .credentials[0].boundProviderId == $provider_id
      and .credentials[0].boundEndpoint == $base
    ' "$settings_body" >/dev/null; then
    break
  fi
  sleep 0.1
done
[[ "$credential_ref" =~ ^credential\.[0-9a-f]{48}$ ]]
if grep -F -- "$fixture_secret" "$settings_body" >/dev/null; then
  echo "canonical Settings v2 leaked the fixture credential" >&2
  exit 1
fi
native_capture_screenshot \
  /tmp/aworkit-native-project-read-credential-settings.png

native_click_at 400 270
native_click_at 1000 550
native_click_at 1000 601
native_click_at 1393 838
wait_for_request_count models 1
native_click_at 1199 552
native_scroll_at 1200 850 420
native_click_at 1180 772
native_select_all
native_type_text 'text, tools'
native_click_at 851 808
native_click_at 1429 580
wait_for_request_count models 2
native_capture_screenshot /tmp/aworkit-native-project-read-model-settings.png

native_click_at 390 325
native_scroll_at 1200 850 -2000
native_click_at 1300 895
native_click_at 1150 946

native_click_at 400 716
native_click_at 1495 339
sleep 0.7
# Focus the GTK folder chooser before sending it pointer/keyboard input. The
# Aworkit sidebar is stable and the unique fixture sorts as the first folder.
native_click_at 600 400
native_click_at 190 296
sleep 0.8
native_click_at 500 333
native_click_at 1172 959
sleep 0.8

native_click_at 400 446
native_click_at 618 478
native_click_at 1485 208

settings_body=""
for _ in {1..120}; do
  if [[ -s "$settings_root/manifest.json" ]]; then
    relative_path=$(jq -r '.documents["settings.desktop"].relative_path // empty' \
      "$settings_root/manifest.json")
    if [[ -n "$relative_path" && -s "$settings_root/$relative_path" ]]; then
      settings_body="$settings_root/$relative_path"
      if jq -e \
        --arg base "$base_url" \
        --arg model "$model_id" \
        --arg root "$project_root" \
        --arg credential "$credential_ref" '
          .schemaVersion == 2
          and (.providers | length == 1)
          and .providers[0].baseUrl == $base
          and .providers[0].models[0].remoteId == $model
          and .providers[0].enabled
          and .providers[0].credentialRef == $credential
          and .providers[0].models[0].enabled
          and ([.providers[0].models[0].capabilities[]] | sort == ["text", "tools"])
          and ([.tools[] | select(.id == "tool.files.read" and .enabled)] | length == 1)
          and (.projects | length == 1)
          and .projects[0].workspace.kind == "local_directory"
          and .projects[0].workspace.location == $root
          and ([.modelTiers[] | select(.id == "tier:balanced" and .resolution.strategy == "exact")] | length == 1)
        ' "$settings_body" >/dev/null; then
        break
      fi
    fi
  fi
  sleep 0.1
done
test -n "$settings_body"

model_local_id=$(jq -er '.providers[0].models[0].id' "$settings_body")
project_id=$(jq -er '.projects[0].id' "$settings_body")
project_name=$(jq -er '.projects[0].name' "$settings_body")
read_tool_snapshot=$(jq -c '.tools[] | select(.id == "tool.files.read")' \
  "$settings_body")
jq -e \
  --arg provider_id "$provider_id" \
  --arg model_local_id "$model_local_id" \
  --arg remote_model "$model_id" \
  --arg credential "$credential_ref" \
  --arg base "$base_url" \
  --arg root "$project_root" '
    .providers[0].baseUrl == $base
    and .providers[0].enabled
    and .providers[0].models[0].remoteId == $remote_model
    and .providers[0].models[0].enabled
    and ([.providers[0].models[0].capabilities[]] | sort == ["text", "tools"])
    and .providers[0].credentialRef == $credential
    and (.credentials | length == 1)
    and .credentials[0].credentialRef == $credential
    and .credentials[0].fieldNames == ["api_key"]
    and .credentials[0].revision == 1
    and .credentials[0].boundProviderId == $provider_id
    and .credentials[0].boundEndpoint == $base
    and ([.tools[] | select(.enabled) | .id] == ["tool.files.read"])
    and ([.tools[] | select(.id == "tool.files.read")][0] | (
      .requiresProject
      and .configuration.authorityMode == "project_files"
      and .configuration.effect == "read"
      and .configuration.maximumBytes == 65536
    ))
    and ([.tools[] | select(.id == "tool.files.search")][0] | (
      .requiresProject
      and .configuration.authorityMode == "project_files"
      and .configuration.effect == "search"
      and .configuration.maximumResults == 512
    ))
    and .projects[0].workspace.location == $root
    and .projects[0].defaultWorkflowId == "workflow.simple-chat"
    and (.projects[0].portableHistoryEnabled | not)
    and ([.modelTiers[] | select(
      .id == "tier:balanced"
      and .resolution.target.providerId == $provider_id
      and .resolution.target.modelId == $model_local_id
    )] | length == 1)
  ' "$settings_body" >/dev/null
if grep -F -- "$fixture_secret" "$settings_body" >/dev/null; then
  echo "canonical Settings v2 leaked the fixture credential" >&2
  exit 1
fi
settings_version=$(jq -er '.documents["settings.desktop"].document_version' \
  "$settings_root/manifest.json")

native_capture_screenshot /tmp/aworkit-native-project-read-tools-settings.png
native_click_at 400 716
native_capture_screenshot /tmp/aworkit-native-project-read-project-settings.png

# Bind only the enabled read tool to the exact four-node Simple Chat Agent.
native_click_at 210 302
sleep 0.8
native_click_at 803 667
native_capture_screenshot /tmp/aworkit-native-project-read-agent-selected.png
native_click_at 1415 604
native_select_all
for workflow_json_fragment in \
  '{' '"modelTierId"' ':' '"tier:balanced"' ',' \
  '"toolIds"' ':' '[' '"tool.files.read"' ']' ',' \
  '"maxTurns"' ':' '2' '}'; do
  native_type_text "$workflow_json_fragment"
done
sleep 1
native_click_at 1415 728
native_click_at 1413 208

workflow_root="$native_temporary_directory/xdg-data/com.aworkit.desktop/runtime/documents/workflows"
workflow_body=""
for _ in {1..120}; do
  if [[ -s "$workflow_root/manifest.json" ]]; then
    workflow_relative_path=$(jq -r '.documents["workflow.simple-chat"].relative_path // empty' \
      "$workflow_root/manifest.json")
    if [[ -n "$workflow_relative_path" && -s "$workflow_root/$workflow_relative_path" ]]; then
      workflow_body="$workflow_root/$workflow_relative_path"
      if jq -e '
        .nodes[1].configuration.modelTierId == "tier:balanced"
        and .nodes[1].configuration.toolIds == ["tool.files.read"]
        and .nodes[1].configuration.maxTurns == 2
      ' "$workflow_body" >/dev/null; then
        break
      fi
    fi
  fi
  sleep 0.1
done
test -n "$workflow_body"
jq -e '
  .schemaVersion == 1
  and .id == "workflow.simple-chat"
  and ([.nodes[].type] == ["input", "agent", "output", "wait"])
  and ([.edges[] | [.source, .target]] == [
    ["input.1", "agent.1"],
    ["agent.1", "output.1"],
    ["output.1", "wait.1"]
  ])
  and .nodes[1].configuration == {
    "modelTierId":"tier:balanced",
    "toolIds":["tool.files.read"],
    "maxTurns":2
  }
' "$workflow_body" >/dev/null
workflow_version=$(jq -er '.documents["workflow.simple-chat"].document_version' \
  "$workflow_root/manifest.json")
native_capture_screenshot /tmp/aworkit-native-project-read-workflow.png

# Select the saved project in the literal Chat composer and run one read/final
# two-turn provider exchange.
native_click_at 210 390
sleep 0.8
native_click_at 590 993
native_click_at 640 1038
native_click_at 760 1038
native_type_text "$tool_prompt"
native_press_key '\uE007'
wait_for_request_count chat.completion 2

history_database="$native_temporary_directory/xdg-data/com.aworkit.desktop/runtime/history/aworkit.sqlite3"
first_reply=""
for _ in {1..160}; do
  first_reply=$(sqlite3 "$history_database" \
    "SELECT json_extract(payload, '$.body') FROM semantic_events WHERE kind='message.assistant' ORDER BY rowid DESC LIMIT 1;" 2>/dev/null || true)
  [[ "$first_reply" == "AWORKIT_TOOL_FINAL: $fixture_marker" ]] && break
  sleep 0.1
done
[[ "$first_reply" == "AWORKIT_TOOL_FINAL: $fixture_marker" ]]

first_chat_id=$(sqlite3 "$history_database" \
  "SELECT json_extract(payload, '$.chatId') FROM semantic_events WHERE kind='chat.started' ORDER BY rowid DESC LIMIT 1;")
first_run_id=$(sqlite3 "$history_database" \
  "SELECT json_extract(payload, '$.runId') FROM semantic_events WHERE kind='chat.started' ORDER BY rowid DESC LIMIT 1;")
[[ "$first_chat_id" =~ ^chat\.[0-9a-f]{40}$ ]]
[[ "$first_run_id" =~ ^run\.[0-9a-f]{40}$ ]]

frozen_record=$(sqlite3 "$history_database" \
  "SELECT json_extract(payload, '$.record') FROM semantic_events WHERE chat_id='chat.frozen-sessions' AND kind='chat.execution-context-frozen' ORDER BY rowid DESC LIMIT 1;")
jq -e \
  --arg chat "$first_chat_id" \
  --arg run "$first_run_id" \
  --arg project_id "$project_id" \
  --arg project_name "$project_name" \
  --arg root "$project_root" \
  --arg provider_id "$provider_id" \
  --arg model_local_id "$model_local_id" \
  --arg remote_model "$model_id" \
  --arg credential "$credential_ref" \
  --argjson tool "$read_tool_snapshot" '
    .context.schemaVersion == 1
    and .context.identity == {"chatId":$chat,"runId":$run}
    and .context.project.projectId == $project_id
    and .context.project.projectName == $project_name
    and .context.project.projectSnapshot.workspace.location == $root
    and .context.project.workspaceKind == "local_directory"
    and .context.project.workspaceBinding.root == $root
    and .context.project.workspaceBinding.identity.canonicalPath == $root
    and (.context.project.projectConfigurationHash | test("^sha256:[0-9a-f]{64}$"))
    and (.context.project.workspaceIdentityHash | test("^sha256:[0-9a-f]{64}$"))
    and .context.workflowId == "workflow.simple-chat"
    and .context.workflowSnapshot.nodes[1].configuration.toolIds == ["tool.files.read"]
    and .context.workflowSnapshot.nodes[1].configuration.maxTurns == 2
    and .context.agentMaximumTurns == 2
    and .context.maximumToolCalls == 8
    and (.context.tools | length == 1)
    and .context.tools[0].toolId == "tool.files.read"
    and (.context.tools[0].toolHash | test("^sha256:[0-9a-f]{64}$"))
    and .context.tools[0].toolSnapshot == $tool
    and .context.providerId == $provider_id
    and .context.modelId == $model_local_id
    and .context.remoteModelId == $remote_model
    and ([.context.modelSnapshot.capabilities[]] | sort == ["text", "tools"])
    and .context.opaqueBinding.credentialRef == $credential
    and .context.opaqueBinding.fieldNames == ["api_key"]
    and .context.opaqueBinding.revision == 1
    and (.contextHash | test("^sha256:[0-9a-f]{64}$"))
  ' <<<"$frozen_record" >/dev/null
frozen_context_hash=$(jq -er '.contextHash' <<<"$frozen_record")
frozen_tool_hash=$(jq -er '.context.tools[0].toolHash' <<<"$frozen_record")
workspace_identity_hash=$(jq -er '.context.project.workspaceIdentityHash' \
  <<<"$frozen_record")

tool_activity=$(sqlite3 "$history_database" \
  "SELECT payload FROM semantic_events WHERE kind='tool.completed' ORDER BY rowid DESC LIMIT 1;")
jq -e \
  --arg context_hash "$frozen_context_hash" \
  --arg tool_hash "$frozen_tool_hash" \
  --arg workspace_hash "$workspace_identity_hash" '
    .callId == "call_read_1"
    and .capabilityId == "tool.files.read"
    and .path == "notes.txt"
    and .status == "completed"
    and (.replayed | not)
    and .frozenContextHash == $context_hash
    and .frozenToolHash == $tool_hash
    and .workspaceIdentityHash == $workspace_hash
    and (.invocationId | test("^[a-z][a-z0-9._:-]+$"))
    and (.outcomeHash | test("^sha256:[0-9a-f]{64}$"))
  ' <<<"$tool_activity" >/dev/null

[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE chat_id='pipeline.tool-invocations' AND kind='pipeline.tool-invocation-prepared';") -eq 1 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE chat_id='pipeline.tool-invocations' AND kind='pipeline.tool-outcome';") -eq 1 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE chat_id='pipeline.tool-invocations' AND kind='pipeline.model-tool-exchange';") -eq 1 ]]
prepared_tool=$(sqlite3 "$history_database" \
  "SELECT json_extract(payload, '$.record') FROM semantic_events WHERE chat_id='pipeline.tool-invocations' AND kind='pipeline.tool-invocation-prepared' LIMIT 1;")
jq -e \
  --arg root "$project_root" \
  --arg run "$first_run_id" '
    .turn == 1
    and .call.callId == "call_read_1"
    and .call.capabilityId == "tool.files.read"
    and .call.name == "aworkit_read_project_file"
    and .call.arguments == {"path":"notes.txt"}
    and .proposal.runId == $run
    and .workspace.root == $root
    and .workspace.identity.canonicalPath == $root
    and .binding.capabilityId == "tool.files.read"
  ' <<<"$prepared_tool" >/dev/null
tool_outcome=$(sqlite3 "$history_database" \
  "SELECT json_extract(payload, '$.record') FROM semantic_events WHERE chat_id='pipeline.tool-invocations' AND kind='pipeline.tool-outcome' LIMIT 1;")
jq -e \
  --arg marker "$fixture_marker" '
    .callId == "call_read_1"
    and .capabilityId == "tool.files.read"
    and .path == "notes.txt"
    and (.isError | not)
    and .result.path == "notes.txt"
    and .result.content == ($marker + "\n")
    and .result.bytes == (($marker + "\n") | utf8bytelength)
    and (.result.contentHash | test("^sha256:[0-9a-f]{64}$"))
  ' <<<"$tool_outcome" >/dev/null

native_capture_screenshot /tmp/aworkit-native-project-read-first-turn.png

# Hard process boundary. The unchanged profile resumes the frozen Chat, but a
# new follow-up is a new provider invocation and must not replay the prior read.
native_hard_restart_application
native_click_at 760 1038
native_type_text "$followup_prompt"
native_press_key '\uE007'
wait_for_request_count chat.completion 3

second_reply=""
for _ in {1..160}; do
  second_reply=$(sqlite3 "$history_database" \
    "SELECT json_extract(payload, '$.body') FROM semantic_events WHERE kind='message.assistant' ORDER BY rowid DESC LIMIT 1;" 2>/dev/null || true)
  [[ "$second_reply" == 'AWORKIT_TOOL_FOLLOWUP: settled context resumed without another tool call' ]] && break
  sleep 0.1
done
[[ "$second_reply" == 'AWORKIT_TOOL_FOLLOWUP: settled context resumed without another tool call' ]]

[[ $(request_count models) -eq 2 ]]
[[ $(request_count chat.completion) -eq 3 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE chat_id='pipeline.tool-invocations' AND kind='pipeline.tool-invocation-prepared';") -eq 1 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE chat_id='pipeline.tool-invocations' AND kind='pipeline.tool-outcome';") -eq 1 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE chat_id='pipeline.tool-invocations' AND kind='pipeline.model-tool-exchange';") -eq 1 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE kind='tool.completed';") -eq 1 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE kind='tool.failed';") -eq 0 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE kind='message.user';") -eq 2 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE kind='message.assistant';") -eq 2 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE kind='execution.failed';") -eq 0 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE chat_id='chat.frozen-sessions' AND kind='chat.execution-context-frozen' AND json_extract(payload, '$.record.contextHash')='$frozen_context_hash';") -eq 1 ]]
[[ $(jq -er '.documents["settings.desktop"].document_version' \
  "$settings_root/manifest.json") -eq "$settings_version" ]]
[[ $(jq -er '.documents["workflow.simple-chat"].document_version' \
  "$workflow_root/manifest.json") -eq "$workflow_version" ]]

jq -s -e \
  --arg model "$model_id" \
  --arg prompt "$tool_prompt" \
  --arg followup "$followup_prompt" \
  --arg marker "$fixture_marker" '
    [.[] | select(.kind == "chat.completion")] as $requests
    | ($requests | length) == 3
      and ([$requests[].model] | all(. == $model))
      and ([$requests[].stream] | all(. == false))
      and ([$requests[].toolChoice] | all(. == "auto"))
      and ([$requests[].tools | length] | all(. == 1))
      and ([$requests[].tools[0].function.name] | all(. == "aworkit_read_project_file"))
      and ([$requests[].tools[0].function.parameters.type] | all(. == "object"))
      and $requests[0].messages == [{"role":"user","content":$prompt}]
      and $requests[1].messages[0] == {"role":"user","content":$prompt}
      and $requests[1].messages[1].content == null
      and $requests[1].messages[1].tool_calls == [{
        "id":"call_read_1",
        "type":"function",
        "function":{
          "name":"aworkit_read_project_file",
          "arguments":"{\"path\":\"notes.txt\"}"
        }
      }]
      and $requests[1].messages[2].role == "tool"
      and $requests[1].messages[2].tool_call_id == "call_read_1"
      and (($requests[1].messages[2].content | fromjson) | (
        .path == "notes.txt"
        and .content == ($marker + "\n")
        and (.contentHash | test("^sha256:[0-9a-f]{64}$"))
      ))
      and $requests[2].messages == [
        {"role":"user","content":$prompt},
        {"role":"assistant","content":("AWORKIT_TOOL_FINAL: " + $marker + "\n")},
        {"role":"user","content":$followup}
      ]
  ' "$request_log" >/dev/null

[[ $(sha256sum "$project_root/notes.txt" | cut -d' ' -f1) == \
  "$project_content_hash_before" ]]
[[ $(sha256sum "$native_temporary_directory/outside.txt" | cut -d' ' -f1) == \
  "$outside_content_hash_before" ]]
for secret_free_file in \
  "$settings_body" \
  "$workflow_body" \
  "$history_database" \
  "$request_log" \
  "$native_temporary_directory/app.log"; do
  if grep -aF -- "$fixture_secret" "$secret_free_file" >/dev/null; then
    echo "credential value appeared outside the operating-system store: $secret_free_file" >&2
    exit 1
  fi
done
native_capture_screenshot /tmp/aworkit-native-project-read-after-restart.png

# Negative authority check: the same tool-bound workflow with No project must
# fail before exposing a provider definition or dispatching a file effect.
native_click_at 220 240
sleep 0.8
native_click_at 760 1038
native_type_text "$tool_prompt"
native_press_key '\uE007'
sleep 1.5
[[ $(request_count chat.completion) -eq 3 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE chat_id='pipeline.tool-invocations' AND kind='pipeline.tool-invocation-prepared';") -eq 1 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE chat_id='pipeline.tool-invocations' AND kind='pipeline.tool-outcome';") -eq 1 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE kind='message.user';") -eq 2 ]]
[[ $(sqlite3 "$history_database" \
  "SELECT count(*) FROM semantic_events WHERE kind='message.assistant';") -eq 2 ]]
native_capture_screenshot /tmp/aworkit-native-project-read-no-project-refusal.png

python3 - \
  /tmp/aworkit-native-project-read-credential-settings.png \
  /tmp/aworkit-native-project-read-tools-settings.png \
  /tmp/aworkit-native-project-read-project-settings.png \
  /tmp/aworkit-native-project-read-workflow.png \
  /tmp/aworkit-native-project-read-first-turn.png \
  /tmp/aworkit-native-project-read-after-restart.png \
  /tmp/aworkit-native-project-read-no-project-refusal.png <<'PY'
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
    if image.width < 1500 or image.height < 1100 or len(colors) < 650 or dominant >= 0.8:
        raise SystemExit(
            f"native screenshot is incomplete: {path.name} {image.width}x{image.height}, "
            f"colors={len(colors)}, dominant={dominant:.1%}"
        )
    print(
        f"{path}: {image.width}x{image.height}; "
        f"colors={len(colors)}; dominant={dominant:.1%}"
    )
PY

# Remove the exact fixture credential after all acceptance evidence is sealed.
# The New Chat above releases the prior frozen credential identity first.
native_click_at 200 1038
sleep 0.7
native_click_at 400 270
native_click_at 1000 550
native_click_at 1000 580
native_click_at 1485 208
for _ in {1..120}; do
  relative_path=$(jq -r '.documents["settings.desktop"].relative_path // empty' \
    "$settings_root/manifest.json")
  current_settings_body="$settings_root/$relative_path"
  jq -e '.providers[0].credentialRef == null' "$current_settings_body" \
    >/dev/null && break
  sleep 0.1
done
native_click_at 400 392
native_click_at 1497 525
native_click_at 270 263
for _ in {1..120}; do
  relative_path=$(jq -r '.documents["settings.desktop"].relative_path // empty' \
    "$settings_root/manifest.json")
  current_settings_body="$settings_root/$relative_path"
  jq -e '(.credentials | length) == 0' "$current_settings_body" \
    >/dev/null && break
  sleep 0.1
done
jq -e '(.credentials | length) == 0' "$current_settings_body" >/dev/null

echo "native project/read-tool acceptance passed: write-only credential + saved project + tool-capable model + frozen read authority + hard-restart no-replay + no-project pre-provider refusal"
