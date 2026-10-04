---
name: comfyui
description: Generate or edit images with the configured ComfyUI server through the ComfyUI bridge plugin.
---

# ComfyUI image generation

Use the ComfyUI bridge plugin when the user asks for an image, a variation, or a
batch of images and a ComfyUI server is configured.

1. Call `comfyui_status` to confirm the endpoint is reachable. If it is not,
   report the endpoint and ask the user to start ComfyUI or correct the plugin
   settings. Do not retry in a loop.
2. Call `comfyui_list_workflows` and pick a workflow id. Never invent one.
3. Call `comfyui_run_workflow` with the chosen id. Change only the inputs the
   request requires, using `<node id>.<input name>` keys. Workflow ids and node
   ids come from the list and the plugin's `workflows.json`.
4. Report the returned image references (`url`, `filename`) exactly as received.
   Do not claim an image exists without a returned reference.

ComfyUI calls queue real GPU work. Prefer one well-specified call over several
speculative ones, and tell the user when a call is still queued after the
timeout instead of resubmitting it.
