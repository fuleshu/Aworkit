import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { chatImagePreview, type ImageAttachment } from "./images";
import "./images.css";

export function ImageAttachmentMenu({
  disabled,
  onFiles,
}: {
  readonly disabled: boolean;
  readonly onFiles: (files: readonly File[]) => void;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const container = useRef<HTMLDivElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  const menuItem = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!open) return;
    menuItem.current?.focus();
    const outside = (event: PointerEvent) => {
      if (!container.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open]);
  return (
    <div
      className="image-attachment-menu"
      ref={container}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          setOpen(false);
          container.current
            ?.querySelector<HTMLButtonElement>("[aria-haspopup]")
            ?.focus();
        }
      }}
    >
      <button
        aria-label="Add attachment"
        aria-haspopup="menu"
        aria-expanded={open}
        title="Add images"
        type="button"
        disabled={disabled}
        onClick={() => setOpen(!open)}
      >
        ＋
      </button>
      {open && !disabled && (
        <div
          className="image-attachment-popup"
          role="menu"
          aria-label="Add attachment"
        >
          <button
            ref={menuItem}
            role="menuitem"
            type="button"
            title="Browse for one or more PNG, JPEG or WebP images"
            onClick={() => {
              picker.current?.click();
              setOpen(false);
            }}
          >
            Add image
          </button>
        </div>
      )}
      <input
        ref={picker}
        className="image-file-picker"
        aria-label="Choose images"
        title="Select one or more images"
        type="file"
        accept="image/png,image/jpeg,image/webp,.png,.jpg,.jpeg,.webp"
        multiple
        disabled={disabled}
        onChange={(event) => {
          onFiles(Array.from(event.target.files ?? []));
          event.target.value = "";
        }}
      />
    </div>
  );
}

export function ImageAttachments({
  images,
  disabled = false,
  onRemove,
}: {
  readonly images: readonly ImageAttachment[];
  readonly disabled?: boolean;
  readonly onRemove?: (index: number) => void;
}): React.JSX.Element | null {
  if (images.length === 0) return null;
  return (
    <div className="chat-image-list" aria-label="Attached images">
      {images.map((image, index) => (
        <ImageThumbnail
          key={`${image.id}-${index}`}
          image={image}
          disabled={disabled}
          onRemove={onRemove === undefined ? undefined : () => onRemove(index)}
        />
      ))}
    </div>
  );
}

function ImageThumbnail({
  image,
  disabled,
  onRemove,
}: {
  readonly image: ImageAttachment;
  readonly disabled: boolean;
  readonly onRemove?: () => void;
}) {
  const [source, setSource] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [fullSource, setFullSource] = useState<string | null>(null);
  useEffect(() => {
    if (!expanded || fullSource !== null) return;
    let current = true;
    void chatImagePreview(image, false)
      .then((value) => {
        if (current) setFullSource(value);
      })
      .catch((failure) => {
        if (current) setError(String(failure));
      });
    return () => {
      current = false;
    };
  }, [expanded, fullSource, image]);
  useEffect(() => {
    let current = true;
    void chatImagePreview(image)
      .then((value) => {
        if (current) setSource(value);
      })
      .catch((failure) => {
        if (current) setError(String(failure));
      });
    return () => {
      current = false;
    };
  }, [image.id, image.mimeType, image.byteLength, image.name]);
  return (
    <div className="chat-image-thumbnail">
      <button
        type="button"
        className="chat-image-open"
        title={error ?? `Preview ${image.name}`}
        aria-label={`Preview ${image.name}`}
        disabled={source === null}
        onClick={() => setExpanded(true)}
      >
        {source === null ? (
          <span>{error === null ? "Loading image…" : "Image unavailable"}</span>
        ) : (
          <img src={source} alt={image.name} />
        )}
        <span className="chat-image-name">{image.name}</span>
      </button>
      {onRemove !== undefined && (
        <button
          type="button"
          className="chat-image-remove"
          aria-label={`Remove ${image.name}`}
          title={`Remove ${image.name}`}
          disabled={disabled}
          onClick={onRemove}
        >
          ×
        </button>
      )}
      {expanded &&
        source !== null &&
        createPortal(
          <ImagePreview
            source={fullSource ?? source}
            name={image.name}
            onClose={() => setExpanded(false)}
          />,
          document.body,
        )}
    </div>
  );
}

/** Native modal semantics supply focus trapping, Escape, and focus restoration. */
function ImagePreview({
  source,
  name,
  onClose,
}: {
  readonly source: string;
  readonly name: string;
  readonly onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    dialog.current?.showModal();
  }, []);
  return (
    <dialog
      className="chat-image-dialog"
      ref={dialog}
      aria-label={name}
      onClose={onClose}
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="chat-image-dialog-header">
        <span>{name}</span>
        <button
          type="button"
          title="Close preview"
          aria-label="Close image preview"
          onClick={onClose}
        >
          ×
        </button>
      </div>
      <img src={source} alt={name} />
    </dialog>
  );
}
