import { expect, it } from "vitest";
import { pastedImageFiles, pasteIntent, type PasteItem } from "./pasteImages";

const image = (name: string, type: string) =>
  new File([new Uint8Array([1, 2, 3, 4])], name, { type });
const item = (kind: string, type: string, file: File | null): PasteItem => ({
  kind,
  type,
  getAsFile: () => file,
});

it("prefers the paste file list so one image never attaches twice", () => {
  const pasted = image("pasted.png", "image/png");
  const files = pastedImageFiles({
    files: [pasted],
    items: [item("file", "image/png", pasted)],
  });
  expect(files).toEqual([pasted]);
});

it("reads the item list when the platform exposes pasted image data only there", () => {
  // The formats the GTK pasteboard reports to WebKit, in WebKit's own order of
  // preference, each named exactly as WebKit names the pasted file.
  const reported: readonly (readonly [string, string])[] = [
    ["image/png", "image.png"],
    ["image/jpeg", "image.jpeg"],
    ["image/gif", "image.gif"],
    ["image/bmp", "image.bmp"],
    ["image/x-icon", "image.ico"],
  ];
  for (const [type, name] of reported) {
    const pasted = image(name, type);
    // WebKit leaves `files` empty for clipboard image data and reports the
    // image only as a file item.
    const files = pastedImageFiles({
      files: [],
      items: [item("file", type, pasted)],
    });
    expect(files.map((file) => [file.type, file.name])).toEqual([[type, name]]);
  }
});

it("skips text items, empty items and files the platform gave no bytes for", () => {
  const pasted = image("image.png", "image/png");
  const files = pastedImageFiles({
    files: [],
    items: [
      item("string", "text/html", null),
      item("string", "text/plain", null),
      item("file", "image/png", null),
      item("file", "", image("report.txt", "text/plain")),
      item("file", "image/png", pasted),
    ],
  });
  expect(files).toEqual([pasted]);
});

it("accepts an untitled item by its stored image extension and ignores other files", () => {
  const untitled = image("clip.png", "");
  const files = pastedImageFiles({ files: [], items: [item("file", "", untitled)] });
  expect(files).toEqual([untitled]);
  expect(
    pastedImageFiles({
      files: [image("notes.txt", "text/plain")],
      items: [item("string", "text/plain", null)],
    }),
  ).toEqual([]);
  expect(pastedImageFiles(null)).toEqual([]);
});

it("asks for the native clipboard when the webview offers nothing at all", () => {
  // WebKitGTK 2.52 hands the page an empty DataTransfer for a clipboard image:
  // no files, no items, and no type list. Nothing here can produce the image,
  // so the composer must read the OS clipboard.
  expect(pasteIntent({ files: [], items: [], types: [] })).toEqual({
    kind: "native",
  });
});

it("leaves text, copied files and an absent clipboard to the default paste", () => {
  expect(pasteIntent(null)).toEqual({ kind: "ignore" });
  expect(
    pasteIntent({ files: [], types: ["text/plain"], getData: () => "hello" }),
  ).toEqual({ kind: "ignore" });
  // A copied non-image file advertises Files; the default paste owns it.
  expect(
    pasteIntent({
      files: [image("notes.txt", "text/plain")],
      types: ["Files", "text/uri-list"],
      getData: () => "",
    }),
  ).toEqual({ kind: "ignore" });
});

it("hands over image files together with any plain text the paste also carried", () => {
  const pasted = image("image.png", "image/png");
  const intent = pasteIntent({
    files: [pasted],
    types: ["Files"],
    getData: () => "caption",
  });
  expect(intent).toEqual({ kind: "files", files: [pasted], text: "caption" });
});
