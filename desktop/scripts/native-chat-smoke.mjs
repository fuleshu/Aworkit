import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";

const endpoint = process.env.AWORKIT_CDP_URL ?? "http://127.0.0.1:9223";
const verifyProviderRuntimeSettings =
  process.env.AWORKIT_VERIFY_PROVIDER_RUNTIME_SETTINGS === "1";
const verifyWebSearchSettings =
  process.env.AWORKIT_VERIFY_WEB_SEARCH_SETTINGS === "1";
const verifyChatHistory = process.env.AWORKIT_VERIFY_CHAT_HISTORY === "1";
const verifyChatSelection =
  process.env.AWORKIT_VERIFY_CHAT_SELECTION === "1";
const screenshotPath = resolve(
  process.env.AWORKIT_SMOKE_SCREENSHOT ??
    "src-tauri/target/native-chat-smoke.png",
);

const targets = await fetch(`${endpoint}/json/list`).then((response) => {
  if (!response.ok) {
    throw new Error(`WebView target discovery failed with HTTP ${response.status}`);
  }
  return response.json();
});
const target = targets.find(
  (candidate) =>
    candidate.type === "page" && candidate.url.startsWith("http://tauri.localhost"),
);
if (target === undefined) {
  throw new Error("No running Aworkit WebView target was found");
}

const socket = new WebSocket(target.webSocketDebuggerUrl);
const pending = new Map();
let messageId = 0;

socket.addEventListener("message", ({ data }) => {
  const message = JSON.parse(data);
  if (message.id === undefined) return;
  const request = pending.get(message.id);
  if (request === undefined) return;
  pending.delete(message.id);
  if (message.error !== undefined) {
    request.reject(new Error(message.error.message));
  } else {
    request.resolve(message.result);
  }
});

await new Promise((resolveOpen, rejectOpen) => {
  socket.addEventListener("open", resolveOpen, { once: true });
  socket.addEventListener(
    "error",
    () => rejectOpen(new Error("Could not connect to the Aworkit WebView")),
    { once: true },
  );
});

function command(method, params = {}) {
  const id = ++messageId;
  return new Promise((resolveCommand, rejectCommand) => {
    pending.set(id, { resolve: resolveCommand, reject: rejectCommand });
    socket.send(JSON.stringify({ id, method, params }));
  });
}

async function evaluate(expression) {
  const result = await command("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (result.exceptionDetails !== undefined) {
    throw new Error(result.exceptionDetails.text ?? "WebView evaluation failed");
  }
  return result.result.value;
}

await command("Runtime.enable");
await command("Page.enable");
await evaluate(`(() => {
  const chatControl = [...document.querySelectorAll("button, a")].find(
    (element) => element.textContent?.trim() === "Chat",
  );
  chatControl?.click();
})()`);

let state;
for (let attempt = 0; attempt < 25; attempt += 1) {
  state = await evaluate(`(() => {
    const bodyText = document.body?.innerText ?? "";
    const composer = document.querySelector("textarea");
    return {
      readyState: document.readyState,
      title: document.title,
      projectionUnavailable: bodyText.includes("Chat projection unavailable"),
      obsoleteSnapshotError:
        bodyText.includes('path: [ "lastSequence" ]') ||
        bodyText.includes('path: [ "timeline" ]'),
      composerPresent: composer !== null,
      composerEnabled: composer !== null && !composer.disabled,
      newChatPresent: bodyText.includes("New Chat"),
      bodyPreview: bodyText.slice(0, 600),
    };
  })()`);
  if (state.projectionUnavailable || state.composerPresent) break;
  await new Promise((resolveWait) => setTimeout(resolveWait, 200));
}

const chatHistoryState = verifyChatHistory
  ? await evaluate(`(async () => {
      const waitFor = async (predicate) => {
        for (let attempt = 0; attempt < 120; attempt += 1) {
          if (predicate()) return true;
          await new Promise((resolveWait) => setTimeout(resolveWait, 125));
        }
        return false;
      };
      const rows = () => [...document.querySelectorAll('.chat-history-row')];
      const row = (chatId) =>
        document.querySelector('[data-chat-id="' + CSS.escape(chatId) + '"]');
      const addedChatIds = (initialIds) =>
        rows()
          .map((entry) => entry.dataset.chatId)
          .filter((chatId) => chatId !== undefined && !initialIds.has(chatId));
      const openMenu = async (chatId) => {
        row(chatId)?.querySelector('.chat-history-more')?.click();
        return waitFor(
          () => row(chatId)?.querySelector('[role="menu"]') !== null,
        );
      };
      const invokeAction = async (chatId, label) => {
        if (!(await openMenu(chatId))) return false;
        const action = [...(row(chatId)?.querySelectorAll('[role="menuitem"]') ?? [])]
          .find((button) => button.textContent?.trim() === label);
        action?.click();
        return action !== undefined;
      };

      const initialIds = new Set(
        rows().map((entry) => entry.dataset.chatId).filter(Boolean),
      );
      const initialCount = initialIds.size;
      const newChat = document.querySelector('button.new-chat');
      newChat?.click();
      const newChatCreated = await waitFor(
        () => addedChatIds(initialIds).length === 1,
      );
      const parentChatId = addedChatIds(initialIds)[0] ?? '';
      const pinInvoked = await invokeAction(parentChatId, 'Pin');
      const pinned = await waitFor(
        () =>
          document.querySelector(
            '[aria-label="pinned"] [data-chat-id="' + CSS.escape(parentChatId) + '"]',
          ) !== null,
      );
      const forkInvoked = await invokeAction(parentChatId, 'Fork');
      const forkCreated = await waitFor(
        () => addedChatIds(initialIds).length === 2,
      );
      const childChatId =
        addedChatIds(initialIds).find((chatId) => chatId !== parentChatId) ?? '';
      const menuOpened = await openMenu(childChatId);
      const activeActions = [...(row(childChatId)?.querySelectorAll('[role="menuitem"]') ?? [])]
        .map((button) => button.textContent?.trim() ?? '');
      document.body.click();
      return {
        initialCount,
        finalCount: rows().length,
        newChatCreated,
        pinInvoked,
        pinned,
        forkInvoked,
        forkCreated,
        menuOpened,
        activeActions,
        addedChatIds: addedChatIds(initialIds),
      };
    })()`)
  : null;

const chatSelectionState = verifyChatSelection
  ? await evaluate(`(async () => {
      const waitFor = async (predicate) => {
        for (let attempt = 0; attempt < 80; attempt += 1) {
          if (predicate()) return true;
          await new Promise((resolveWait) => setTimeout(resolveWait, 50));
        }
        return false;
      };
      const rows = [...document.querySelectorAll('.chat-history-row')];
      const active = rows.find(
        (row) => row.querySelector('.chat-history-link')?.getAttribute('aria-current') === 'page',
      );
      const target = rows.find(
        (row) => row !== active && !row.querySelector('.chat-history-link')?.disabled,
      );
      const activeId = active?.dataset.chatId ?? null;
      const targetId = target?.dataset.chatId ?? null;
      const initialWorkflowControl = document.querySelector(
        '[aria-label="Workflow for the first Chat input"]',
      );
      const initialWorkflowLabel =
        initialWorkflowControl?.selectedOptions[0]?.textContent?.trim() ?? '';
      const initialChatContext =
        document.querySelector('.chat-title-line > span')?.textContent ?? '';
      const initialWorkflowMatchesHeader =
        initialWorkflowControl?.disabled !== true ||
        (initialWorkflowLabel.length > 0 && initialChatContext.includes(initialWorkflowLabel));
      const historyViewport = document.querySelector('.navigation-history');
      const originalHistoryScrollTop = historyViewport?.scrollTop ?? 0;
      if (historyViewport !== null) {
        historyViewport.scrollTop = Math.max(
          0,
          Math.floor((historyViewport.scrollHeight - historyViewport.clientHeight) / 2),
        );
        await new Promise((resolveFrame) => requestAnimationFrame(resolveFrame));
      }
      const positionedRow = rows.find((candidate) => {
        const bounds = candidate.getBoundingClientRect();
        const viewport = historyViewport?.getBoundingClientRect();
        return viewport === undefined || (bounds.bottom > viewport.top && bounds.top < viewport.bottom);
      });
      const positionedLink = positionedRow?.querySelector('.chat-history-link');
      const positionedBounds = positionedLink?.getBoundingClientRect();
      const contextX = Math.round((positionedBounds?.left ?? 24) + 8);
      const contextY = Math.round((positionedBounds?.top ?? 24) + 8);
      positionedLink?.dispatchEvent(new MouseEvent('contextmenu', {
        bubbles: true,
        cancelable: true,
        clientX: contextX,
        clientY: contextY,
      }));
      const contextMenuOpened = await waitFor(
        () => positionedRow?.querySelector('[role="menu"]') !== null,
      );
      const contextMenu = positionedRow?.querySelector('[role="menu"]');
      const contextBounds = contextMenu?.getBoundingClientRect();
      const expectedLeft = contextBounds === undefined
        ? contextX
        : Math.min(Math.max(contextX, 8), window.innerWidth - contextBounds.width - 8);
      const expectedTop = contextBounds === undefined
        ? contextY
        : Math.min(Math.max(contextY, 8), window.innerHeight - contextBounds.height - 8);
      const menuPositionedAfterScroll = contextBounds !== undefined &&
        Math.abs(contextBounds.left - expectedLeft) <= 1 &&
        Math.abs(contextBounds.top - expectedTop) <= 1;
      document.body.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }));
      if (historyViewport !== null) historyViewport.scrollTop = originalHistoryScrollTop;
      const startedAt = performance.now();
      target?.querySelector('.chat-history-link')?.click();
      const selected = targetId !== null && await waitFor(
        () =>
          document.querySelector(
            '[data-chat-id="' + CSS.escape(targetId) + '"] .chat-history-link',
          )?.getAttribute('aria-current') === 'page',
      );
      await new Promise((resolveFrame) => requestAnimationFrame(resolveFrame));
      const elapsedMs = Math.round(performance.now() - startedAt);
      const errorDialogPresent = document.querySelector('[role="alertdialog"]') !== null;
      const workflowControl = document.querySelector(
        '[aria-label="Workflow for the first Chat input"]',
      );
      const workflowLabel = workflowControl?.selectedOptions[0]?.textContent?.trim() ?? '';
      const chatContext = document.querySelector('.chat-title-line > span')?.textContent ?? '';
      const workflowMatchesHeader = workflowControl?.disabled !== true ||
        (workflowLabel.length > 0 && chatContext.includes(workflowLabel));
      if (activeId !== null) {
        document.querySelector(
          '[data-chat-id="' + CSS.escape(activeId) + '"] .chat-history-link',
        )?.click();
        await waitFor(
          () =>
            document.querySelector(
              '[data-chat-id="' + CSS.escape(activeId) + '"] .chat-history-link',
            )?.getAttribute('aria-current') === 'page',
        );
      }
      return {
        activeId,
        targetId,
        initialWorkflowLabel,
        initialChatContext,
        initialWorkflowMatchesHeader,
        selected,
        elapsedMs,
        errorDialogPresent,
        contextMenuOpened,
        menuPositionedAfterScroll,
        workflowLabel,
        chatContext,
        workflowMatchesHeader,
      };
    })()`)
  : null;

const runDetailsState = await evaluate(`(() => {
  const inspector = document.querySelector('[aria-label="Run details"]');
  const content = inspector?.querySelector('.run-details-content');
  return {
    present: inspector !== null,
    entireRunPresent: inspector?.textContent?.includes('Entire run') === true,
    executionLogPresent: inspector?.textContent?.includes('Execution log') === true,
    detailsContainsRawJson: content?.querySelector('pre') !== null,
  };
})()`);
const modelCallOutputState = await evaluate(`(() => {
  const block = document.querySelector('.model-call-block');
  const output = [...(block?.querySelectorAll('details.model-call-data') ?? [])]
    .find((element) => element.querySelector('summary')?.textContent?.trim() === 'Output');
  const json = output?.querySelector('pre')?.textContent ?? null;
  if (json === null) {
    return { blockPresent: block !== null, outputPresent: false, validJson: false, duplicateTextKinds: [] };
  }
  try {
    const value = JSON.parse(json);
    const textKinds = Array.isArray(value)
      ? value.flatMap((entry) =>
          entry !== null && typeof entry === 'object' && typeof entry.kind === 'string' &&
          (typeof entry.text === 'string' || typeof entry.data === 'string')
            ? [entry.kind + ':' + (typeof entry.text === 'string' ? 'text' : 'data')]
            : [],
        )
      : [];
    const seen = new Set();
    const duplicates = new Set();
    for (const kind of textKinds) {
      if (seen.has(kind)) duplicates.add(kind);
      seen.add(kind);
    }
    return {
      blockPresent: true,
      outputPresent: true,
      validJson: true,
      duplicateTextKinds: [...duplicates],
    };
  } catch {
    return { blockPresent: true, outputPresent: true, validJson: false, duplicateTextKinds: [] };
  }
})()`);
const approvalState = await evaluate(`(() => {
  const cards = [...document.querySelectorAll('.approval-card')];
  const projected = cards.map((card) => ({
    status: card.querySelector('.status')?.textContent?.trim() ?? null,
    approvePresent: [...card.querySelectorAll('button')].some(
      (button) => button.textContent?.trim() === 'Approve',
    ),
    rejectPresent: [...card.querySelectorAll('button')].some(
      (button) => button.textContent?.trim() === 'Reject',
    ),
  }));
  return {
    cards: projected,
    staleActions: projected.filter(
      (card) =>
        card.status !== 'pending' &&
        (card.approvePresent || card.rejectPresent),
    ).length,
  };
})()`);
const rawRunDetailsState = await evaluate(`(async () => {
  const inspector = document.querySelector('[aria-label="Run details"]');
  const tabs = [...(inspector?.querySelectorAll('[role="tab"]') ?? [])];
  const rawTab = tabs.find((element) => element.textContent?.trim() === 'Raw JSON');
  rawTab?.click();
  await new Promise((resolveFrame) => requestAnimationFrame(() => requestAnimationFrame(resolveFrame)));
  const raw = inspector?.querySelector('.run-details-json');
  const result = {
    tabPresent: rawTab !== undefined,
    jsonPresent: raw !== null,
    scoped: raw?.textContent?.includes('"scope"') === true,
  };
  const detailsTab = tabs.find((element) => element.textContent?.trim() === 'Details');
  detailsTab?.click();
  await new Promise((resolveFrame) => requestAnimationFrame(resolveFrame));
  return result;
})()`);
const layoutState = await evaluate(`(() => {
  const viewportHeight = document.documentElement.clientHeight;
  const rect = (selector) => {
    const element = document.querySelector(selector);
    if (element === null) return null;
    const bounds = element.getBoundingClientRect();
    return { top: bounds.top, bottom: bounds.bottom, height: bounds.height };
  };
  const scrollState = (selector) => {
    const element = document.querySelector(selector);
    if (element === null) return null;
    return {
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
      overflowY: getComputedStyle(element).overflowY,
    };
  };
  const composer = rect('[aria-label="Chat composer"]');
  return {
    viewportHeight,
    bodyScrollHeight: document.body.scrollHeight,
    shell: rect('.desktop-shell'),
    chat: rect('.chat-layout'),
    composer,
    composerFullyVisible:
      composer !== null && composer.top >= 0 && composer.bottom <= viewportHeight + 1,
    timeline: scrollState('.timeline-scroll'),
    inspector: rect('[aria-label="Run details"]'),
    runDetails: scrollState('.run-details-content'),
  };
})()`);
const providerRuntimeSettingsState = verifyProviderRuntimeSettings
  ? await evaluate(`(async () => {
      const settingsControl = [...document.querySelectorAll("button, a")].find(
        (element) => element.textContent?.includes("Settings") === true,
      );
      settingsControl?.click();
      let timeout = null;
      let toolOutput = null;
      for (let attempt = 0; attempt < 25; attempt += 1) {
        timeout = document.querySelector('[id$="-request-timeout-seconds"]');
        toolOutput = document.querySelector('[id$="-maximum-tool-output-bytes"]');
        if (timeout !== null && toolOutput !== null) break;
        await new Promise((resolveWait) => setTimeout(resolveWait, 200));
      }
      return {
        settingsControlPresent: settingsControl !== undefined,
        timeoutPresent: timeout !== null,
        timeoutValue: timeout?.value ?? null,
        timeoutMinimum: timeout?.min ?? null,
        timeoutMaximum: timeout?.max ?? null,
        toolOutputPresent: toolOutput !== null,
        toolOutputValue: toolOutput?.value ?? null,
        toolOutputMinimum: toolOutput?.min ?? null,
        toolOutputMaximum: toolOutput?.max ?? null,
      };
    })()`)
  : null;
const webSearchSettingsState = verifyWebSearchSettings
  ? await evaluate(`(async () => {
      const settingsControl = [...document.querySelectorAll("button, a")].find(
        (element) => element.textContent?.includes("Settings") === true,
      );
      settingsControl?.click();
      let toolsControl = null;
      for (let attempt = 0; attempt < 40; attempt += 1) {
        toolsControl = [...document.querySelectorAll("button")].find(
          (element) => element.textContent?.trim().startsWith("Tools") === true,
        );
        if (toolsControl !== undefined && toolsControl !== null) break;
        await new Promise((resolveWait) => setTimeout(resolveWait, 125));
      }
      toolsControl?.click();
      let backend = null;
      for (let attempt = 0; attempt < 40; attempt += 1) {
        backend = document.getElementById('tool.web_search-backend');
        if (backend !== null) break;
        await new Promise((resolveWait) => setTimeout(resolveWait, 125));
      }
      const maximumResults = document.getElementById('tool.web_search-maximum-results');
      const keylessRescue = document.getElementById('tool.web_search-keyless-rescue');
      const deepseekUrl = document.getElementById('tool.web_search-deepseek-url');
      const deepseekModel = document.getElementById('tool.web_search-deepseek-model');
      const deepseekOutputTokens = document.getElementById(
        'tool.web_search-deepseek-output-tokens',
      );
      const credential = document.getElementById('tool.web_search-provider-credential');
      backend?.closest('.settings-record')?.scrollIntoView({ block: 'start' });
      await new Promise((resolveFrame) =>
        requestAnimationFrame(() => requestAnimationFrame(resolveFrame)),
      );
      return {
        settingsControlPresent: settingsControl !== undefined,
        toolsControlPresent: toolsControl !== undefined && toolsControl !== null,
        backendPresent: backend !== null,
        backendOptions: [...(backend?.options ?? [])].map((option) => option.value),
        maximumResultsValue: maximumResults?.value ?? null,
        maximumResultsMaximum: maximumResults?.max ?? null,
        keylessRescuePresent: keylessRescue !== null,
        deepseekUrlValue: deepseekUrl?.value ?? null,
        deepseekModelValue: deepseekModel?.value ?? null,
        deepseekOutputTokensValue: deepseekOutputTokens?.value ?? null,
        credentialPresent: credential !== null,
        controlsWithoutTooltips: [
          backend,
          maximumResults,
          keylessRescue,
          deepseekUrl,
          deepseekModel,
          deepseekOutputTokens,
          credential,
        ].filter((control) => control !== null && !control.title).length,
      };
    })()`)
  : null;
state = {
  ...state,
  runDetailsState,
  modelCallOutputState,
  approvalState,
  rawRunDetailsState,
  layoutState,
  providerRuntimeSettingsState,
  webSearchSettingsState,
  chatHistoryState,
  chatSelectionState,
};

const screenshot = await command("Page.captureScreenshot", {
  format: "png",
  fromSurface: true,
});
await mkdir(dirname(screenshotPath), { recursive: true });
await writeFile(screenshotPath, Buffer.from(screenshot.data, "base64"));
socket.close();

const failures = [];
if (state.title !== "Aworkit") failures.push(`unexpected title: ${state.title}`);
if (state.projectionUnavailable) failures.push("Chat projection is unavailable");
if (state.obsoleteSnapshotError) failures.push("obsolete Chat snapshot fields are required");
if (!state.composerPresent) failures.push("Chat composer was not rendered");
if (!state.composerEnabled) failures.push("Chat composer is disabled");
if (!state.newChatPresent) failures.push("New Chat control was not rendered");
if (verifyChatHistory) {
  const history = state.chatHistoryState;
  if (!history?.newChatCreated)
    failures.push("New Chat did not create a separate history entry");
  if (!history?.pinInvoked || !history?.pinned)
    failures.push("Chat history Pin did not move the selected Chat into Pinned");
  if (!history?.forkInvoked || !history?.forkCreated)
    failures.push("Chat history Fork did not create and select a separate child Chat");
  if (!history?.menuOpened)
    failures.push("The selected Chat context menu could not be opened");
  for (const action of ["Pin", "Fork", "Delete"]) {
    if (!history?.activeActions.includes(action))
      failures.push(`The selected Chat context menu has no ${action} action`);
  }
}
if (verifyChatSelection) {
  const selection = state.chatSelectionState;
  if (!selection?.selected)
    failures.push("An existing Chat history row could not be selected");
  if ((selection?.elapsedMs ?? Number.POSITIVE_INFINITY) > 2_000)
    failures.push(
      `Chat history selection took ${selection?.elapsedMs ?? "unknown"} ms`,
    );
  if (selection?.errorDialogPresent)
    failures.push("Selecting historical Chat data opened an error dialog");
  if (!selection?.contextMenuOpened || !selection?.menuPositionedAfterScroll)
    failures.push("Chat context menu is not viewport-aligned after history scrolling");
  if (!selection?.workflowMatchesHeader)
    failures.push(
      `Historical Chat workflow selector does not match its frozen header (${selection?.workflowLabel ?? "unknown"})`,
    );
  if (!selection?.initialWorkflowMatchesHeader)
    failures.push(
      `Initially selected Chat workflow does not match its frozen header (${selection?.initialWorkflowLabel ?? "unknown"})`,
    );
}
if (!state.runDetailsState.present) failures.push("Run details was not rendered");
if (!state.runDetailsState.entireRunPresent)
  failures.push("whole-run details were not rendered");
if (!state.runDetailsState.executionLogPresent)
  failures.push("Run details has no execution log");
if (state.runDetailsState.detailsContainsRawJson)
  failures.push("Details contains a raw JSON block");
if (state.modelCallOutputState.outputPresent && !state.modelCallOutputState.validJson)
  failures.push("Model-call Output is not valid formatted JSON");
if (state.modelCallOutputState.duplicateTextKinds.length > 0)
  failures.push(
    `Model-call Output still contains streamed fragments for: ${state.modelCallOutputState.duplicateTextKinds.join(", ")}`,
  );
if (state.approvalState.staleActions > 0)
  failures.push("settled or cancelled approval cards still expose decision actions");
if (!state.rawRunDetailsState.tabPresent)
  failures.push("Raw JSON tab was not rendered");
if (!state.rawRunDetailsState.jsonPresent || !state.rawRunDetailsState.scoped)
  failures.push("Raw JSON does not contain scoped Run details");
const boundedBottom = (bounds) =>
  bounds !== null && bounds.bottom <= state.layoutState.viewportHeight + 1;
if (!boundedBottom(state.layoutState.shell))
  failures.push("desktop shell extends below the WebView");
if (!boundedBottom(state.layoutState.chat))
  failures.push("Chat extends below the WebView");
if (!state.layoutState.composerFullyVisible)
  failures.push("Chat composer is clipped by the WebView");
if (!boundedBottom(state.layoutState.inspector))
  failures.push("Run details extends below the WebView");
if (!["auto", "scroll"].includes(state.layoutState.timeline?.overflowY))
  failures.push("Chat timeline has no independent vertical scroll contract");
if (!["auto", "scroll"].includes(state.layoutState.runDetails?.overflowY))
  failures.push("Run details has no independent vertical scroll contract");
if (verifyProviderRuntimeSettings) {
  const settings = state.providerRuntimeSettingsState;
  if (!settings?.settingsControlPresent)
    failures.push("Settings navigation was not rendered");
  if (!settings?.timeoutPresent || settings.timeoutValue !== "300")
    failures.push("Provider request timeout does not render its 300 second default");
  if (settings?.timeoutMinimum !== "1" || settings?.timeoutMaximum !== "3600")
    failures.push("Provider request timeout bounds are incorrect");
  if (!settings?.toolOutputPresent || settings.toolOutputValue !== "65536")
    failures.push("Maximum tool output does not render its 65536 byte default");
  if (settings?.toolOutputMinimum !== "1024" || settings?.toolOutputMaximum !== "524288")
    failures.push("Maximum tool output bounds are incorrect");
}
if (verifyWebSearchSettings) {
  const settings = state.webSearchSettingsState;
  const expectedBackends = [
    "automatic",
    "keyless",
    "duckduckgo",
    "searxng",
    "exa",
    "parallel",
    "firecrawl",
    "tavily",
    "brave",
    "keenable",
    "xai",
    "deepseek",
  ];
  if (
    !settings?.settingsControlPresent ||
    !settings?.toolsControlPresent ||
    !settings?.backendPresent
  )
    failures.push("Web-search Settings were not rendered in the native WebView");
  if (JSON.stringify(settings?.backendOptions) !== JSON.stringify(expectedBackends))
    failures.push("Web-search Settings do not expose the complete provider list");
  const maximumResults = Number(settings?.maximumResultsValue ?? Number.NaN);
  if (
    !Number.isInteger(maximumResults) ||
    maximumResults < 1 ||
    maximumResults > 100 ||
    settings?.maximumResultsMaximum !== "100"
  )
    failures.push("Web-search maximum-result defaults or bounds are incorrect");
  if (!settings?.keylessRescuePresent)
    failures.push("Web-search one-shot keyless rescue control was not rendered");
  if (
    settings?.deepseekUrlValue !== "https://api.deepseek.com" ||
    settings?.deepseekModelValue !== "deepseek-v4-flash" ||
    settings?.deepseekOutputTokensValue !== "4096"
  )
    failures.push("Paid DeepSeek search settings were not rendered with their defaults");
  if (!settings?.credentialPresent)
    failures.push("Web-search credential lease selector was not rendered");
  if ((settings?.controlsWithoutTooltips ?? 1) !== 0)
    failures.push("A native web-search Settings control has no tooltip");
}

console.log(
  JSON.stringify(
    { ok: failures.length === 0, failures, screenshotPath, state },
    null,
    2,
  ),
);
if (failures.length > 0) process.exitCode = 1;
