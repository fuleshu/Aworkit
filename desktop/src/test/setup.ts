import "@testing-library/jest-dom/vitest";

// Normalize a broken web-storage global. Some Node runners shadow jsdom's
// Storage with an inert empty object (the `--localstorage-file` flag), which
// breaks tests that call localStorage.clear(). Provide a correct in-memory
// Storage only when the environment's implementation is missing its methods.
if (typeof window !== "undefined") {
  const storage = window.localStorage as unknown;
  const missingClear =
    storage === undefined ||
    storage === null ||
    typeof (storage as { clear?: unknown }).clear !== "function";
  if (missingClear) {
    const memory = new Map<string, string>();
    const polyfill: Storage = {
      get length(): number {
        return memory.size;
      },
      clear(): void {
        memory.clear();
      },
      getItem(key: string): string | null {
        return memory.has(key) ? (memory.get(key) ?? null) : null;
      },
      key(index: number): string | null {
        return [...memory.keys()][index] ?? null;
      },
      removeItem(key: string): void {
        memory.delete(key);
      },
      setItem(key: string, value: string): void {
        memory.set(key, String(value));
      },
    };
    Object.defineProperty(window, "localStorage", {
      configurable: true,
      value: polyfill,
    });
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      value: polyfill,
    });
  }
}

class TestResizeObserver {
  public observe(): void {
    /* deterministic layout stub */
  }
  public unobserve(): void {
    /* deterministic layout stub */
  }
  public disconnect(): void {
    /* deterministic layout stub */
  }
}
Object.defineProperty(globalThis, "ResizeObserver", {
  configurable: true,
  value: TestResizeObserver,
});
if (typeof window !== "undefined") {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: (query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => undefined,
      removeListener: () => undefined,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
      dispatchEvent: () => false,
    }),
  });
  Object.defineProperty(HTMLElement.prototype, "scrollTo", {
    configurable: true,
    value: () => undefined,
  });
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
    configurable: true,
    value: () => undefined,
  });
  Object.defineProperty(window, "requestAnimationFrame", {
    configurable: true,
    value: (callback: FrameRequestCallback) =>
      window.setTimeout(() => callback(performance.now()), 0),
  });
}
