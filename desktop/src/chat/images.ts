import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";

export const maxImageBytes = 5 * 1024 * 1024;
export const maxImageContextBytes = 12 * 1024 * 1024;
export const maxImages = 20;
export const imageAttachmentSchema = z
  .object({
    id: z.string().regex(/^[a-f0-9]{64}$/),
    name: z.string().min(1).max(255),
    mimeType: z.enum(["image/png", "image/jpeg", "image/webp"]),
    byteLength: z.number().int().positive().max(maxImageBytes),
  })
  .strict();
export type ImageAttachment = z.infer<typeof imageAttachmentSchema>;
export const imageAttachmentsSchema = z
  .array(imageAttachmentSchema)
  .max(maxImages);

// Only the browser preview uses this ephemeral store. Native Chat always reads
// its durable, validated profile store through the dedicated image IPC port.
const previewImages = new Map<string, string>();

export function validateImageSelection(
  images: readonly ImageAttachment[],
): void {
  if (
    images.length > maxImages ||
    images.reduce((sum, image) => sum + image.byteLength, 0) >
      maxImageContextBytes
  )
    throw new Error("Add up to 20 images, totalling at most 12 MiB.");
}

function fileDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error(`Could not read ${file.name}`));
    reader.onload = () => resolve(String(reader.result));
    reader.readAsDataURL(file);
  });
}

/** A file picker and OS clipboard both supply Files to this same import path. */
export async function importChatImage(file: File): Promise<ImageAttachment> {
  if (
    !/^(image\/(png|jpeg|webp))$/.test(file.type) &&
    !/\.(png|jpe?g|webp)$/i.test(file.name)
  )
    throw new Error("Choose PNG, JPEG or WebP images.");
  if (file.size === 0 || file.size > maxImageBytes)
    throw new Error(
      `${file.name || "Image"} must be between 1 byte and 5 MiB.`,
    );
  const dataUrl = await fileDataUrl(file);
  const data = dataUrl.slice(dataUrl.indexOf(",") + 1);
  const name = file.name || "Pasted image.png";
  if ("__TAURI_INTERNALS__" in window)
    return imageAttachmentSchema.parse(
      await invoke("chat_image_import", { name, data }),
    );
  const bytes = Uint8Array.from(atob(data), (character) =>
    character.charCodeAt(0),
  );
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  const id = [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
  const attachment = imageAttachmentSchema.parse({
    id,
    name,
    mimeType: file.type,
    byteLength: file.size,
  });
  previewImages.set(id, dataUrl);
  return attachment;
}

export async function chatImagePreview(
  image: ImageAttachment,
  thumbnail = true,
): Promise<string> {
  if ("__TAURI_INTERNALS__" in window)
    return invoke(thumbnail ? "chat_image_thumbnail" : "chat_image_preview", {
      image,
    });
  const preview = previewImages.get(image.id);
  if (preview === undefined) throw new Error("Image preview is unavailable");
  return preview;
}
