#!/usr/bin/env bash
# Shared Broadway/WebDriver primitives for literal GTK/WebKit acceptance gates.
# The caller owns scenario setup, diagnostics, assertions, and the EXIT trap.
# shellcheck shell=bash

: "${native_binary_path:?native_binary_path is required}"
: "${native_temporary_directory:?native_temporary_directory is required}"

native_application_pid=""
native_broadway_pid=""
native_driver_pid=""
native_webdriver_session_id=""
native_webdriver=""

native_start_driver() {
  geckodriver --port "$native_driver_port" \
    >"$native_temporary_directory/geckodriver.log" 2>&1 &
  native_driver_pid=$!
}

native_start_broadway() {
  broadwayd --address=127.0.0.1 --port="$native_broadway_port" \
    ":$native_broadway_display" \
    >>"$native_temporary_directory/broadway.log" 2>&1 &
  native_broadway_pid=$!
}

native_wait_for_transports() {
  for _ in {1..80}; do
    if curl --fail --silent \
      "http://127.0.0.1:$native_broadway_port/" >/dev/null \
      && curl --fail --silent \
        "http://127.0.0.1:$native_driver_port/status" >/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

native_start_application() {
  GDK_BACKEND=broadway \
    BROADWAY_DISPLAY=":$native_broadway_display" \
    XDG_DATA_HOME="$native_temporary_directory/xdg-data" \
    XDG_CONFIG_HOME="$native_temporary_directory/xdg-config" \
    WEBKIT_DISABLE_COMPOSITING_MODE=1 \
    "$native_binary_path" \
    >>"$native_temporary_directory/app.log" 2>&1 &
  native_application_pid=$!
}

native_create_webdriver_session() {
  local session_response
  session_response=$(curl --fail --silent --request POST \
    --header 'Content-Type: application/json' \
    --data '{"capabilities":{"alwaysMatch":{"moz:firefoxOptions":{"args":["-headless"]}}}}' \
    "http://127.0.0.1:$native_driver_port/session")
  native_webdriver_session_id=$(jq -er '.value.sessionId' <<<"$session_response")
  native_webdriver="http://127.0.0.1:$native_driver_port/session/$native_webdriver_session_id"
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data '{"width":1600,"height":1250}' \
    "$native_webdriver/window/rect" >/dev/null
  native_open_broadway_url "initial=1"
}

native_open_broadway_url() {
  local query=${1:-}
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data "{\"url\":\"http://127.0.0.1:$native_broadway_port/?$query\"}" \
    "$native_webdriver/url" >/dev/null
}

native_wait_for_canvas() {
  local ready=false
  for _ in {1..100}; do
    ready=$(curl --fail --silent --request POST \
      --header 'Content-Type: application/json' \
      --data '{"script":"const canvas=document.querySelector(\"canvas\"); return canvas!==null && canvas.width>=1400 && canvas.height>=1000;","args":[]}' \
      "$native_webdriver/execute/sync" | jq -r '.value')
    [[ "$ready" == "true" ]] && break
    sleep 0.1
  done
  [[ "$ready" == "true" ]]
  kill -0 "$native_application_pid"
}

native_release_actions() {
  curl --fail --silent --request DELETE \
    "$native_webdriver/actions" >/dev/null
}

native_click_at() {
  local x=$1
  local y=$2
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data "{\"actions\":[{\"type\":\"pointer\",\"id\":\"mouse\",\"parameters\":{\"pointerType\":\"mouse\"},\"actions\":[{\"type\":\"pointerMove\",\"duration\":0,\"origin\":\"viewport\",\"x\":$x,\"y\":$y},{\"type\":\"pointerDown\",\"button\":0},{\"type\":\"pointerUp\",\"button\":0}]}]}" \
    "$native_webdriver/actions" >/dev/null
  native_release_actions
  sleep 0.2
}

native_type_text() {
  local value=$1
  local payload
  payload=$(jq -nc --arg value "$value" \
    '{actions:[{type:"key",id:"keyboard",actions:($value|explode|map(([.]|implode) as $character|[{type:"keyDown",value:$character},{type:"pause",duration:12},{type:"keyUp",value:$character},{type:"pause",duration:12}])|add)}]}')
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data "$payload" "$native_webdriver/actions" >/dev/null
  native_release_actions
  sleep 0.35
}

native_press_key() {
  local value=$1
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data "{\"actions\":[{\"type\":\"key\",\"id\":\"keyboard\",\"actions\":[{\"type\":\"keyDown\",\"value\":\"$value\"},{\"type\":\"keyUp\",\"value\":\"$value\"}]}]}" \
    "$native_webdriver/actions" >/dev/null
  native_release_actions
  sleep 0.15
}

native_press_control_key() {
  local value=$1
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data "{\"actions\":[{\"type\":\"key\",\"id\":\"keyboard\",\"actions\":[{\"type\":\"keyDown\",\"value\":\"\\uE009\"},{\"type\":\"keyDown\",\"value\":\"$value\"},{\"type\":\"keyUp\",\"value\":\"$value\"},{\"type\":\"keyUp\",\"value\":\"\\uE009\"}]}]}" \
    "$native_webdriver/actions" >/dev/null
  native_release_actions
  sleep 0.25
}

native_select_all() {
  native_press_control_key a
}

native_scroll_at() {
  local x=$1
  local y=$2
  local delta_y=$3
  curl --fail --silent --request POST --header 'Content-Type: application/json' \
    --data "{\"actions\":[{\"type\":\"wheel\",\"id\":\"wheel\",\"actions\":[{\"type\":\"scroll\",\"x\":$x,\"y\":$y,\"deltaX\":0,\"deltaY\":$delta_y,\"duration\":200,\"origin\":\"viewport\"}]}]}" \
    "$native_webdriver/actions" >/dev/null
  native_release_actions
  sleep 0.3
}

native_open_route() {
  native_press_control_key "$1"
  sleep 0.5
}

native_capture_screenshot() {
  local target=$1
  curl --fail --silent "$native_webdriver/screenshot" \
    | jq -er '.value' \
    | base64 --decode >"$target"
}

native_hard_restart_application() {
  kill "$native_application_pid"
  wait "$native_application_pid" 2>/dev/null || true
  native_application_pid=""
  for _ in {1..50}; do
    ! kill -0 "$native_broadway_pid" 2>/dev/null && break
    sleep 0.02
  done
  if kill -0 "$native_broadway_pid" 2>/dev/null; then
    kill "$native_broadway_pid" 2>/dev/null || true
  fi
  wait "$native_broadway_pid" 2>/dev/null || true
  native_broadway_pid=""
  native_start_broadway
  native_wait_for_transports
  native_start_application
  native_open_broadway_url "restart=1"
  sleep 1.5
  native_wait_for_canvas
}

native_delete_webdriver_session() {
  if [[ -n "$native_webdriver_session_id" && -n "$native_driver_pid" ]]; then
    curl --silent --request DELETE \
      "http://127.0.0.1:$native_driver_port/session/$native_webdriver_session_id" \
      >/dev/null || true
    native_webdriver_session_id=""
  fi
}

native_stop_processes() {
  native_delete_webdriver_session
  local process_id
  for process_id in \
    "$native_application_pid" "$native_driver_pid" "$native_broadway_pid"; do
    if [[ -n "$process_id" ]] && kill -0 "$process_id" 2>/dev/null; then
      kill "$process_id" 2>/dev/null || true
      wait "$process_id" 2>/dev/null || true
    fi
  done
}
