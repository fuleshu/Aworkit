import { expect, it } from "vitest";
import { dragCarriesFiles, droppedImageFiles } from "./dropImages";

const image = (name: string, type: string) =>
  new File([new Uint8Array([1, 2, 3, 4])], name, { type });

it("claims a drag that advertises files and leaves an internal drag alone", () => {
  expect(dragCarriesFiles({ types: ["Files"], files: [] })).toBe(true);
  expect(dragCarriesFiles({ types: ["files"], files: [] })).toBe(true);
  // Some renderers hide the format list while the drag is over the target.
  expect(dragCarriesFiles({ types: [], files: [] })).toBe(true);
  // The Workflow designer drags its own node type and must keep its drop target.
  expect(
    dragCarriesFiles({ types: ["application/x-aworkit-node"], files: [] }),
  ).toBe(false);
  expect(dragCarriesFiles({ types: ["text/plain"], files: [] })).toBe(false);
  expect(dragCarriesFiles(null)).toBe(false);
});

it("returns the dropped images and ignores files the import path cannot store", () => {
  const png = image("photo.png", "image/png");
  const notes = image("notes.txt", "text/plain");
  expect(
    droppedImageFiles({ types: ["Files"], files: [png, notes] }),
  ).toEqual([png]);
  // A file manager drag often reports no MIME type; the extension decides.
  expect(
    droppedImageFiles({ types: ["Files"], files: [image("scan.jpeg", "")] }),
  ).toHaveLength(1);
  expect(droppedImageFiles({ types: ["Files"], files: [] })).toEqual([]);
  expect(droppedImageFiles(null)).toEqual([]);
});
