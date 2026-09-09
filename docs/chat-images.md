# Chat images

## Image tools

Enable **Image read** and **Screenshot** in **Settings → Tools**, bind them to the
workflow's Agent, and start a new Chat with a vision-capable model. Existing Chats
keep their frozen tool selection and model capabilities.

- `aworkit_read_image({"path":"assets/example.png"})` reads an image relative to
  the selected project, using the same project boundary as file tools.
- `aworkit_screenshot({"operation":"list"})` lists available Windows windows and
  monitors with target identifiers, titles and physical pixel bounds.
- `aworkit_screenshot({"operation":"capture","target":"<identifier from list>"})`
  captures that selected target through the Chat's normal tool approval policy.

Screenshots currently capture **visible desktop pixels on Windows**. Keep the
chosen window visible and unobscured; overlapping windows will appear in the
capture. Minimized windows are excluded, partially offscreen windows are clipped,
and unavailable targets must be listed again. No mouse or keyboard input is sent.
Other platforms report that screenshot capture is unsupported.

Both tools return clickable image previews and send the actual image to the next
model request. Their results retain immutable references in settled tool history,
so replay and reopening do not read the source or take another screenshot. They
use the same image limits and storage as attachments below. Screenshot target
listing itself does not require vision; reading or capturing an image does.

From `desktop`, run `node scripts/native-image-tools-smoke.mjs` to exercise the native editor, approvals,
window capture, provider image bytes, history restart and previews against an
isolated profile and local fixture provider.

## Attachments

Use **+ → Add image** in the Chat composer to select one or more files, or paste
an image from the OS clipboard with the normal paste shortcut. Attachments appear
as removable thumbnails before sending and remain visible in the submitted user
message. Click a thumbnail to open its preview. Image-only messages are supported.

In **Settings → Providers**, enable **Vision (image input)** for the chosen model
and save. Use a model that actually supports vision, then start a new Chat: model
capabilities are frozen when a Chat starts. Discovery preserves advertised vision
capabilities, including OpenAI-compatible `supports_vision` and input modalities.
Enabling the switch does not add vision to a text-only model.

The initial implementation accepts PNG, JPEG and WebP, up to 5 MiB per image,
8000 pixels per side, and 20 images / 12 MiB in the accumulated Chat image context.
Provider-specific limits can be lower. Images are not silently resized or omitted.

Small thumbnails are decoded serially outside the WebView. Full-resolution bytes
are loaded for the expanded preview and model submission.

Images are validated and copied into the active profile's `runtime/images`
directory under their SHA-256 identity. Chat history, forks, pending commands,
frozen authority and checkpoints contain only compact image references. Removing
or moving the source file does not affect saved Chats. Image blobs currently share
the profile's lifetime; deleting a Chat does not garbage-collect shared blobs.

Only an approved model request resolves those references into image bytes. The
same mapping is used for ordinary completions and model/tool turns:

| Provider | Image representation |
| --- | --- |
| OpenAI-compatible Chat Completions | `image_url` content parts with base64 data URLs |
| Anthropic Messages | `image` content blocks with a base64 source |
| Gemini generateContent | `inlineData` parts with MIME type and base64 data |

Wire contracts follow the official [OpenAI vision guide](https://developers.openai.com/api/docs/guides/images-vision),
[Anthropic vision guide](https://platform.claude.com/docs/en/build-with-claude/vision),
and [Gemini image guide](https://ai.google.dev/gemini-api/docs/image-understanding).

Regression coverage includes provider HTTP requests for all three protocols,
image-only and multiple-image turns, UI paste/picker import, rejected-submit retry,
history reopen/fork, corrupt-image rejection, and an image larger than the history
commit limit crossing the full native authority pipeline without embedding its
bytes in durable records. `desktop/scripts/native-image-fixture.mjs` starts an
isolated debug profile and local provider for native picker/clipboard verification.
The `AWORKIT_QA_PROFILE` override is available only in debug builds.
