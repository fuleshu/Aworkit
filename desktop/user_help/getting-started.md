# Getting started: providers and model tiers

Aworkit needs one thing to answer anything: a model provider. You add the
provider once, map the portable model tiers to it, and every workflow can then
refer to a tier by name instead of a hard-coded model.

## Step 1 — Add a model provider

Open **Settings → Providers & models**.

1. **Add a provider.** Pick a preset if one matches your service (it fills in a
   starting point) or add a blank provider. Nothing is enabled or stored yet —
   the provider is a draft until you save.
2. **Name it.** The name is what you will see in Settings and in model-tier
   mapping.
3. **Choose the protocol.** The request protocol the native adapter speaks
   (for example OpenAI-compatible, Anthropic or Gemini).
4. **Set the Base URL.** The absolute HTTP(S) API base, without embedded
   credentials, query or fragment.
5. **Bind a credential.** A provider API key is never typed into the provider
   itself. It is stored as a credential and the provider references it. Only
   credentials of the right shape, unbound or bound to this exact provider and
   endpoint, are eligible.
6. **Add concrete models.** Either **fetch** the catalog from the draft and
   merge the models you want, or add a model by hand: a display name, the exact
   remote model id sent to the API, its context window, and if the model accepts
   images, mark it as vision-capable. A model must be **enabled** to become
   eligible for tier resolution.
7. **Enable the provider and save.** Enabling is availability, not authority —
   nothing runs until a workflow actually selects it.

Use the provider **Test** action to confirm the endpoint answers. If the model
catalog is not reachable, the failure is reported against that provider and the
rest of Settings stays usable.

## Step 2 — Map the model tiers

Open **Settings → Model tiers**. Aworkit ships four standard portable tiers:

| Tier | Intended use |
| --- | --- |
| `tier:fast` | Quick, cheap answers |
| `tier:simple` | Light classification and short replies |
| `tier:balanced` | The default for most agents and plans |
| `tier:quality` | Harder reasoning where cost and latency matter less |

A tier resolves to a configured model in one of these ways:

- **Exact** — one provider and model target.
- **Fallback** — an ordered list of at least two targets; the first eligible one
  is used.
- **Policy** — a candidate list plus a preference of *quality*, *latency* or
  *cost*.

A tier left **unconfigured** is not an error until a workflow needs it; the run
then reports that the tier has no exact resolution instead of silently
substituting another model.

You can also create **custom tiers** (`tier:custom-…`) for workflow-specific
needs. Workflows reference tiers by id, so changing which model a tier points to
never requires editing a workflow.

## Step 3 — Start a Chat

Return to **Chat**, pick a workflow in the composer (start with **Standard**),
optionally pick a project and an approval mode, type your message and send. Open
**Run details** on the right to watch the steps.

Next: [Workflows](workflows.md) explains the built-in and example workflows, and
[Tools](tools.md) explains what the agent can call.
