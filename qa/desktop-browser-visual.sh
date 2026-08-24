#!/usr/bin/env bash
# Exercises the production frontend bundle in Firefox. Browser Preview may
# persist local drafts, but every native effect must be refused explicitly.
set -Eeuo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
desktop_root="$repository_root/desktop"

for command in firefox geckodriver curl jq base64 python3; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "desktop browser acceptance requires $command" >&2
    exit 1
  fi
done
if ! python3 -c 'import PIL' >/dev/null 2>&1; then
  echo "desktop browser acceptance requires the Python Pillow package" >&2
  exit 1
fi

(
  cd "$desktop_root"
  ./node_modules/.bin/tsc --noEmit
  ./node_modules/.bin/vite build
)

temporary_directory=$(mktemp -d)
preview_pid=""
driver_pid=""
session_id=""
webdriver=""
cleanup_browser_qa() {
  if [[ -n "$session_id" && "$session_id" != "null" && -n "$webdriver" ]]; then
    curl --silent --request DELETE "$webdriver" >/dev/null || true
  fi
  for process_id in "$driver_pid" "$preview_pid"; do
    if [[ -n "$process_id" ]] && kill -0 "$process_id" 2>/dev/null; then
      kill "$process_id" 2>/dev/null || true
      wait "$process_id" 2>/dev/null || true
    fi
  done
  rm -r -- "$temporary_directory"
}
trap cleanup_browser_qa EXIT

(
  cd "$desktop_root"
  ./node_modules/.bin/vite preview --host 127.0.0.1 --port 4173
) >"$temporary_directory/vite.log" 2>&1 &
preview_pid=$!
geckodriver --port 4444 >"$temporary_directory/geckodriver.log" 2>&1 &
driver_pid=$!

for _ in {1..80}; do
  if curl --fail --silent http://127.0.0.1:4444/status >/dev/null \
    && curl --fail --silent http://127.0.0.1:4173/ >/dev/null; then
    break
  fi
  sleep 0.1
done
curl --fail --silent http://127.0.0.1:4444/status >/dev/null
curl --fail --silent http://127.0.0.1:4173/ >/dev/null

session_response=$(curl --fail --silent --request POST \
  --header 'Content-Type: application/json' \
  --data '{"capabilities":{"alwaysMatch":{"moz:firefoxOptions":{"args":["-headless"]}}}}' \
  http://127.0.0.1:4444/session)
session_id=$(jq -er '.value.sessionId' <<<"$session_response")
webdriver="http://127.0.0.1:4444/session/$session_id"

webdriver_execute() {
  local script=$1
  jq -nc --arg script "$script" '{script:$script,args:[]}' \
    | curl --fail --silent --request POST \
        --header 'Content-Type: application/json' \
        --data-binary @- "$webdriver/execute/sync"
}

webdriver_execute_arg() {
  local script=$1
  local argument=$2
  jq -nc --arg script "$script" --arg argument "$argument" \
    '{script:$script,args:[$argument]}' \
    | curl --fail --silent --request POST \
        --header 'Content-Type: application/json' \
        --data-binary @- "$webdriver/execute/sync"
}

wait_for_true() {
  local script=$1
  local actual=false
  for _ in {1..80}; do
    actual=$(webdriver_execute "$script" | jq -r '.value')
    [[ "$actual" == "true" ]] && break
    sleep 0.1
  done
  [[ "$actual" == "true" ]]
}

click_button() {
  local label=$1
  webdriver_execute_arg '
    const label = arguments[0];
    const candidates = [...document.querySelectorAll("button")];
    const button = candidates.find((candidate) => candidate.textContent.trim() === label)
      ?? candidates.find((candidate) => candidate.textContent.trim().endsWith(label));
    if (!button) throw new Error(`button not found: ${label}`);
    if (button.disabled) throw new Error(`button disabled: ${label}`);
    button.click();
    return true;
  ' "$label" >/dev/null
}

click_settings_section() {
  local label=$1
  webdriver_execute_arg '
    const label = arguments[0];
    const navigation = document.querySelector("[aria-label=\"Settings sections\"]");
    const button = [...navigation.querySelectorAll("button")]
      .find((candidate) => candidate.querySelector("span")?.textContent.trim() === label);
    if (!button) throw new Error(`settings section not found: ${label}`);
    button.click();
    return true;
  ' "$label" >/dev/null
}

set_control_value() {
  local selector=$1
  local value=$2
  webdriver_execute_arg '
    const [selector, value] = arguments[0].split("\u001f");
    const control = document.querySelector(selector);
    if (!control) throw new Error(`control not found: ${selector}`);
    const prototype = control instanceof HTMLTextAreaElement
      ? HTMLTextAreaElement.prototype
      : control instanceof HTMLSelectElement
        ? HTMLSelectElement.prototype
        : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(prototype, "value").set.call(control, value);
    control.dispatchEvent(new Event("input", { bubbles: true }));
    control.dispatchEvent(new Event("change", { bubbles: true }));
    return true;
  ' "$selector"$'\037'"$value" >/dev/null
}

capture_screenshot() {
  local target=$1
  curl --fail --silent "$webdriver/screenshot" \
    | jq -er '.value' \
    | base64 --decode >"$target"
}

curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"width":1440,"height":940}' "$webdriver/window/rect" >/dev/null
curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"url":"http://127.0.0.1:4173/"}' "$webdriver/url" >/dev/null

wait_for_true 'return document.querySelector(".chat-view-header h1")?.textContent === "New Chat";'
chat_state=$(webdriver_execute '
  const rect = (selector) => document.querySelector(selector)?.getBoundingClientRect();
  const attachment = document.querySelector("[aria-label=\"Add attachment references\"]");
  return {
    title: document.querySelector(".chat-view-header h1")?.textContent,
    navigation: Math.round(rect(".navigation-pane").width),
    header: Math.round(rect(".chat-view-header").height),
    inspector: Math.round(rect(".evidence-inspector").width),
    phase: document.querySelector(".run-status")?.textContent.trim(),
    emptyTimeline: document.querySelector(".timeline-empty")?.textContent.includes("No messages yet"),
    attachmentDisabled: attachment?.disabled === true,
    attachmentHonest: attachment?.title === "Attachments are unsupported in this build",
    horizontal: document.documentElement.scrollWidth <= window.innerWidth,
    appearanceReady: document.documentElement.dataset.appearanceReady === "true",
    fabricated: /Project Atlas|Release readiness|Repository Engineer/.test(document.body.innerText),
  };
')
jq -e '
  .value.title == "New Chat"
  and .value.navigation == 208
  and .value.header == 48
  and (.value.inspector >= 319 and .value.inspector <= 321)
  and .value.phase == "Draft"
  and .value.emptyTimeline
  and .value.attachmentDisabled
  and .value.attachmentHonest
  and .value.horizontal
  and .value.appearanceReady
  and (.value.fabricated | not)
' <<<"$chat_state" >/dev/null

# Preview must reject provider-backed Chat execution and keep the unsent draft.
set_control_value '[aria-label="Chat input"]' 'browser preview check'
click_button 'Send'
wait_for_true '
  return document.querySelector(".command-banner")?.textContent
    .includes("browser Preview did not contact a provider") === true;
'
capture_screenshot /tmp/aworkit-desktop-browser-chat.png

# The workflow editor must mutate and save canonical documents, while keeping
# native Run limited to the exact supported graph.
click_button 'Workflows'
wait_for_true 'return document.querySelector(".workflow-editor h1")?.textContent === "Simple Chat";'
workflow_initial=$(webdriver_execute '
  const exact = (name) => [...document.querySelectorAll("button")]
    .find((button) => button.textContent.trim() === name);
  return {
    nodes: document.querySelectorAll(".react-flow__node").length,
    edges: document.querySelectorAll(".react-flow__edge").length,
    nodeTypes: document.querySelectorAll(".node-type-grid button:not(:disabled)").length,
    connectableHandles: document.querySelectorAll(".react-flow__handle.connectable").length,
    saveDisabled: exact("Save")?.disabled,
    runDisabled: exact("Run")?.disabled,
    importEnabled: exact("Import JSON")?.disabled === false,
    exportEnabled: exact("Export")?.disabled === false,
    validateEnabled: [...document.querySelectorAll("button")]
      .find((button) => button.textContent.trim().startsWith("Validate"))?.disabled === false,
    transitionEnabled: exact("Add transition")?.disabled === false,
    status: document.querySelector(".surface-toolbar .status")?.textContent.trim(),
  };
')
jq -e '
  .value.nodes == 4 and .value.edges == 3 and .value.nodeTypes == 10
  and .value.connectableHandles > 0 and .value.saveDisabled
  and (.value.runDisabled | not) and .value.importEnabled and .value.exportEnabled
  and .value.validateEnabled and .value.transitionEnabled
  and .value.status == "Simple Chat executable"
' <<<"$workflow_initial" >/dev/null

set_control_value '.properties-pane input[title="Edit the workflow display name"]' 'Simple Chat QA'
wait_for_true '
  const save = [...document.querySelectorAll("button")]
    .find((button) => button.textContent.trim() === "Save");
  const run = [...document.querySelectorAll("button")]
    .find((button) => button.textContent.trim() === "Run");
  return document.querySelector(".workflow-editor h1")?.textContent === "Simple Chat QA"
    && save?.disabled === false && run?.disabled === true;
'
click_button 'Save'
wait_for_true '
  return document.querySelector(".surface-toolbar .toolbar-actions")?.textContent.includes("Version 2")
    && document.querySelector(".workflow-editor > .command-banner")?.textContent.includes("Workflow saved by the trusted core.");
'

webdriver_execute '
  const button = document.querySelector("[aria-label=\"Add Tool node\"]");
  if (!button || button.disabled) throw new Error("Tool node action is unavailable");
  button.click();
  return true;
' >/dev/null
wait_for_true '
  const save = [...document.querySelectorAll("button")]
    .find((button) => button.textContent.trim() === "Save");
  return document.querySelectorAll(".react-flow__node").length === 5
    && document.querySelector(".surface-toolbar .status")?.textContent.trim() === "Editable · Not runnable"
    && save?.disabled === false;
'
click_button 'Save'
wait_for_true 'return document.querySelector(".surface-toolbar .toolbar-actions")?.textContent.includes("Version 3") === true;'
click_button 'Delete node'
wait_for_true 'return document.querySelectorAll(".react-flow__node").length === 4;'
click_button 'Save'
wait_for_true '
  const run = [...document.querySelectorAll("button")]
    .find((button) => button.textContent.trim() === "Run");
  return document.querySelector(".surface-toolbar .toolbar-actions")?.textContent.includes("Version 4")
    && document.querySelector(".surface-toolbar .status")?.textContent.trim() === "Simple Chat executable"
    && run?.disabled === false;
'
capture_screenshot /tmp/aworkit-desktop-browser-workflow.png

# Settings v2 has exactly ten functional domains. Exercise every editable
# domain and every browser-refused native effect without accepting a fake result.
click_button 'Settings'
wait_for_true 'return document.querySelector(".settings-workspace h1")?.textContent === "Settings";'
settings_navigation=$(webdriver_execute '
  const navigation = document.querySelector("[aria-label=\"Settings sections\"]");
  return [...navigation.querySelectorAll("button")].map((button) => ({
    label: button.querySelector("span")?.textContent.trim(),
    description: button.querySelector("small")?.textContent.trim(),
    enabled: !button.disabled,
  }));
')
jq -e '
  .value | length == 10
  and all(.[]; .enabled and (.label | length > 0) and (.description | length > 0))
  and ([.[].label] == [
    "Providers & models", "Model tiers", "Credentials", "Tools", "Extensions",
    "MCP servers", "External agents", "Data & sessions", "Projects", "Appearance"
  ])
' <<<"$settings_navigation" >/dev/null

# Providers & concrete models: editable presets, exact draft discovery/test,
# and no browser network claim.
click_button 'Add'
wait_for_true 'return document.querySelector("#settings-panel-providers .provider-editor h3") !== null;'
set_control_value '#settings-panel-providers input[title^="Absolute HTTP"]' 'http://127.0.0.1:9/v1'
click_button 'Add model'
set_control_value '#settings-panel-providers input[title^="Human-readable name shown in model-tier"]' 'Browser model'
set_control_value '#settings-panel-providers input[title^="Exact model identifier"]' 'browser-model'
webdriver_execute '
  const panel = document.querySelector("#settings-panel-providers");
  const switches = [...panel.querySelectorAll("input[type=checkbox]")];
  for (const checkbox of switches) if (!checkbox.checked) checkbox.click();
  return switches.length === 2 && switches.every((checkbox) => checkbox.checked);
' | jq -e '.value' >/dev/null
click_button 'Discover models'
wait_for_true 'return document.querySelector("#settings-panel-providers .provider-detail")?.textContent.includes("browser Preview made no network request") === true;'
click_button 'Test'
wait_for_true 'return document.querySelector("#settings-panel-providers .provider-detail")?.textContent.includes("Provider tests require the native desktop runtime") === true;'

click_settings_section 'Model tiers'
wait_for_true 'return document.querySelectorAll("#settings-panel-model_tiers .tier-record").length === 4;'
set_control_value '#settings-panel-model_tiers select[id="tier:balanced-strategy"]' 'exact'
wait_for_true 'return document.querySelector("#settings-panel-model_tiers select[id=\"tier:balanced-exact\"]") !== null;'

click_settings_section 'Credentials'
click_button 'Add credential'
set_control_value '#credential-label' 'Browser write-only check'
set_control_value '#credential-field-0-value' 'must-never-persist'
click_button 'Store credential'
wait_for_true 'return document.querySelector("#settings-panel-credentials .field-error")?.textContent.includes("browser Preview stored no secret") === true;'
click_button 'Cancel'

click_settings_section 'Tools'
wait_for_true 'return document.querySelectorAll("#settings-panel-tools .settings-record").length === 5;'
webdriver_execute '
  const records = [...document.querySelectorAll("#settings-panel-tools .settings-record")];
  const executable = new Set(["tool.files.read", "tool.files.search"]);
  for (const record of records) {
    const id = record.querySelector("code")?.textContent;
    const checkbox = record.querySelector("input[type=checkbox]");
    if (!id || !checkbox) throw new Error("tool execution control is missing");
    if (executable.has(id) && checkbox.disabled)
      throw new Error(`${id} must remain bindable in Simple Chat`);
    if (!executable.has(id) && (!checkbox.disabled || checkbox.checked))
      throw new Error(`${id} falsely presents Simple Chat execution readiness`);
  }
  const hostShell = records.find((record) => record.querySelector("code")?.textContent === "tool.shell.host");
  if (!hostShell?.textContent.includes("cannot be bound to an executable Simple Chat workflow"))
    throw new Error("host shell execution limitation is not explicit");
  const test = [...hostShell.querySelectorAll("button")]
    .find((button) => button.textContent.trim() === "Probe adapter only");
  if (!test || test.disabled) throw new Error("host shell adapter test is unavailable");
  test.click();
  return true;
' >/dev/null
wait_for_true 'return document.querySelector("#settings-panel-tools .provider-detail")?.textContent.includes("browser Preview executed no adapter") === true;'

click_settings_section 'Extensions'
wait_for_true '
  const button = [...document.querySelectorAll("#settings-panel-extensions button")]
    .find((candidate) => candidate.textContent.trim() === "Discover manifest…");
  return button?.disabled === false && document.querySelectorAll("#settings-panel-extensions .settings-record").length === 0;
'
click_button 'Discover manifest…'

click_settings_section 'MCP servers'
click_button 'Add server'
set_control_value '#settings-panel-mcp input[title^="Exact executable"]' '/usr/bin/mcp-preview-fixture'
wait_for_true '
  const execution = [...document.querySelectorAll("#settings-panel-mcp input[type=checkbox]")]
    .find((checkbox) => checkbox.parentElement?.textContent.includes("Simple Chat execution not available"));
  return execution?.disabled === true && execution.checked === false;
'
click_button 'Discover and test'
wait_for_true 'return document.querySelector("#settings-panel-mcp .provider-detail")?.textContent.includes("browser Preview started no process and made no connection") === true;'

click_settings_section 'External agents'
click_button 'Add agent'
wait_for_true '
  const execution = [...document.querySelectorAll("#settings-panel-external_agents input[type=checkbox]")]
    .find((checkbox) => checkbox.parentElement?.textContent.includes("Simple Chat execution not available"));
  return execution?.disabled === true && execution.checked === false;
'
click_button 'Start handshake'
wait_for_true 'return document.querySelector("#settings-panel-external_agents .provider-detail")?.textContent.includes("browser Preview started no process") === true;'

click_settings_section 'Data & sessions'
webdriver_execute '
  const ids = [
    "portable-history-capability",
    "portable-directory",
    "detailed-capture",
    "capture-retention",
    "history-retention",
  ];
  const controls = ids.map((id) => document.querySelector(`#${id}`));
  return controls.every((control) => control?.disabled === true)
    && document.querySelector("#portable-history-capability")?.checked === false
    && document.querySelector("#detailed-capture")?.checked === false
    && document.querySelector("#settings-panel-data")?.textContent.includes("not available") === true;
' | jq -e '.value' >/dev/null

click_settings_section 'Projects'
wait_for_true '
  const add = [...document.querySelectorAll("#settings-panel-projects button")]
    .find((button) => button.textContent.trim() === "Add folder…");
  return add?.disabled === false && document.querySelectorAll("#settings-panel-projects .settings-record").length === 0;
'
click_button 'Add folder…'

click_settings_section 'Appearance'
webdriver_execute '
  const dark = [...document.querySelectorAll("#settings-panel-appearance input[type=radio]")]
    .find((radio) => radio.parentElement?.textContent.includes("Dark"));
  dark.click();
  return document.documentElement.dataset.appearance === "dark";
' | jq -e '.value' >/dev/null
set_control_value '#appearance-font-scale' '1.15'

settings_effects=$(webdriver_execute '
  const body = document.querySelector(".settings-workspace").textContent;
  const projectedColor = (variable) => {
    const probe = document.createElement("span");
    probe.style.color = `var(${variable})`;
    document.body.append(probe);
    const color = getComputedStyle(probe).color;
    probe.remove();
    return color;
  };
  const components = (color) => color.match(/[0-9.]+/g).slice(0, 3).map(Number);
  const luminance = (color) => components(color)
    .map((component) => component / 255)
    .map((value) => value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4)
    .reduce((sum, value, index) => sum + value * [0.2126, 0.7152, 0.0722][index], 0);
  const contrast = (foreground, background) => {
    const values = [luminance(foreground), luminance(background)].sort((left, right) => right - left);
    return (values[0] + 0.05) / (values[1] + 0.05);
  };
  const bodyStyle = getComputedStyle(document.body);
  const controlStyle = getComputedStyle(document.querySelector("#appearance-font-scale"));
  const tokenText = projectedColor("--aw-text");
  const tokenWindow = projectedColor("--aw-window");
  return {
    sectionCount: document.querySelectorAll(".settings-v2-panel").length,
    providers: document.querySelectorAll("#settings-panel-providers .provider-editor").length,
    tiers: document.querySelectorAll("#settings-panel-model_tiers .tier-record").length,
    credentialsRefused: body.includes("browser Preview stored no secret"),
    tools: document.querySelectorAll("#settings-panel-tools .settings-record").length,
    toolRefused: body.includes("browser Preview executed no adapter"),
    extensionDiscoveryReal: [...document.querySelectorAll("#settings-panel-extensions button")]
      .some((button) => button.textContent.trim() === "Discover manifest…" && !button.disabled),
    mcpRefused: body.includes("browser Preview started no process and made no connection"),
    agentRefused: body.includes("External-agent probes require the native desktop runtime"),
    unsupportedExecutionTruthful: ["tool.files.edit", "tool.shell.host", "tool.python.host"]
      .every((id) => {
        const record = [...document.querySelectorAll("#settings-panel-tools .settings-record")]
          .find((candidate) => candidate.querySelector("code")?.textContent === id);
        return record?.querySelector("input[type=checkbox]")?.disabled === true
          && record.textContent.includes("not executable in Simple Chat");
      })
      && [...document.querySelectorAll("#settings-panel-mcp input[type=checkbox]")]
        .some((control) => control.disabled && control.parentElement?.textContent.includes("Simple Chat execution not available"))
      && [...document.querySelectorAll("#settings-panel-external_agents input[type=checkbox]")]
        .some((control) => control.disabled && control.parentElement?.textContent.includes("Simple Chat execution not available")),
    inactiveDataControls: [
      "portable-history-capability",
      "portable-directory",
      "detailed-capture",
      "capture-retention",
      "history-retention",
    ].every((id) => document.querySelector(`#${id}`)?.disabled === true),
    noActivePortableOrCapture: document.querySelector("#portable-history-capability")?.checked === false
      && document.querySelector("#detailed-capture")?.checked === false,
    projectPickerReal: [...document.querySelectorAll("#settings-panel-projects button")]
      .some((button) => button.textContent.trim() === "Add folder…" && !button.disabled),
    dark: document.documentElement.dataset.appearance === "dark",
    scale: document.documentElement.style.getPropertyValue("--aw-font-scale"),
    bodyUsesTextToken: bodyStyle.color === tokenText,
    bodyUsesWindowToken: bodyStyle.backgroundColor === tokenWindow,
    controlUsesTextToken: controlStyle.color === tokenText,
    textWindowContrast: contrast(tokenText, tokenWindow),
    unsupportedCopy: /unsupported in this (rescue )?build/i.test(body),
  };
')
jq -e '
  .value.sectionCount == 10 and .value.providers == 1 and .value.tiers == 4
  and .value.credentialsRefused and .value.tools == 5 and .value.toolRefused
  and .value.extensionDiscoveryReal and .value.mcpRefused and .value.agentRefused
  and .value.unsupportedExecutionTruthful
  and .value.inactiveDataControls and .value.noActivePortableOrCapture
  and .value.projectPickerReal and .value.dark and .value.scale == "1.15"
  and .value.bodyUsesTextToken and .value.bodyUsesWindowToken
  and .value.controlUsesTextToken and .value.textWindowContrast >= 7
  and (.value.unsupportedCopy | not)
' <<<"$settings_effects" >/dev/null

click_button 'Save configuration'
wait_for_true '
  const save = [...document.querySelectorAll("button")]
    .find((button) => button.textContent.trim() === "Save configuration");
  return document.querySelector(".settings-toolbar-actions")?.textContent.includes("Version 2 · saved")
    && save?.disabled === true;
'
capture_screenshot /tmp/aworkit-desktop-browser-settings.png

# Compact navigation remains keyboard-resizable and the responsive shell must
# not create horizontal document scrolling.
webdriver_execute '
  const splitter = document.querySelector("[aria-label=\"Resize navigation pane\"]");
  splitter.focus();
  splitter.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
  return true;
' >/dev/null
wait_for_true 'return document.querySelector("[aria-label=\"Resize navigation pane\"]")?.getAttribute("aria-valuenow") === "216";'
webdriver_execute '
  document.activeElement.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }));
  return true;
' >/dev/null
wait_for_true 'return document.querySelector("[aria-label=\"Resize navigation pane\"]")?.getAttribute("aria-valuenow") === "208";'

curl --fail --silent --request POST --header 'Content-Type: application/json' \
  --data '{"width":760,"height":700}' "$webdriver/window/rect" >/dev/null
responsive=$(webdriver_execute '
  return {
    navigation: Math.round(document.querySelector(".navigation-pane").getBoundingClientRect().width),
    horizontal: document.documentElement.scrollWidth <= window.innerWidth,
  };
')
jq -e '.value.navigation == 44 and .value.horizontal' <<<"$responsive" >/dev/null

python3 - \
  /tmp/aworkit-desktop-browser-chat.png \
  /tmp/aworkit-desktop-browser-workflow.png \
  /tmp/aworkit-desktop-browser-settings.png <<'PY'
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
        raise SystemExit(
            f"{path.name} appears blank: colors={len(colors)}, dominant={dominant:.1%}"
        )
    print(f"{path}: colors={len(colors)}, dominant={dominant:.1%}")
PY

echo "desktop browser acceptance passed: editable/savable workflow, ten functional Settings domains, honest Preview refusals, responsive layout"
