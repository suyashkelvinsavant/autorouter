// Reusable copy-to-clipboard button.
//
// Two variants:
//   - `inline` (default): tiny icon button, sits next to a value (URL, id)
//   - `block`:  full-width pill that says "Copy" with the icon
//
// Every copy shows a global toast via `useToast()`. The Tauri WebView2
// sometimes denies `navigator.clipboard` for non-secure contexts, so the
// `writeText` call is wrapped in `try/catch` and silently no-ops on failure
// (the user already has the value in the UI; we don't gate them on the
// clipboard).

import { useEffect, useRef, useState, type ReactNode } from "react";
import { IconCopy } from "./Icons";
import { useToast } from "./Toast";

interface CopyButtonProps {
  /** The literal text to write to the clipboard. */
  text: string;
  /** Optional label shown next to the icon (block variant only). */
  label?: string;
  /** Tooltip + accessible label. Defaults to "Copy to clipboard". */
  title?: string;
  /** Icon size. */
  size?: "sm" | "md";
  /** Inline icon button (default) or full-width "Copy" pill. */
  variant?: "inline" | "block";
  /** Toast message on success. Defaults to "Copied". */
  successMsg?: string;
  /** Toast message on failure. Defaults to "Copy failed". */
  failureMsg?: string;
  className?: string;
  /** For block variant: replace the default icon+label content. */
  children?: ReactNode;
}

export function CopyButton({
  text,
  label,
  title,
  size = "md",
  variant = "inline",
  successMsg = "Copied",
  failureMsg = "Copy failed",
  className,
  children,
}: CopyButtonProps) {
  const { show } = useToast();
  const [flashing, setFlashing] = useState(false);
  const flashTimer = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (flashTimer.current !== null) {
        window.clearTimeout(flashTimer.current);
        flashTimer.current = null;
      }
    };
  }, []);

  const onCopy = async (e: React.MouseEvent) => {
    // Don't trigger parent click handlers (e.g. a card with onClick).
    e.stopPropagation();
    e.preventDefault();
    let ok = false;
    try {
      if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(text);
        ok = true;
      }
    } catch {
      ok = false;
    }
    if (ok) {
      show(successMsg, "ok");
      setFlashing(true);
      if (flashTimer.current !== null) window.clearTimeout(flashTimer.current);
      flashTimer.current = window.setTimeout(() => {
        flashTimer.current = null;
        setFlashing(false);
      }, 1000);
    } else {
      // Silent fail per the spec — show an error toast so the operator
      // knows nothing landed in the clipboard.
      show(failureMsg, "err");
    }
  };

  const classes = [
    "copy-button",
    `copy-button-${variant}`,
    `copy-button-${size}`,
    flashing ? "success" : "",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");

  const ariaLabel = title ?? "Copy to clipboard";

  if (variant === "block") {
    // Block variant accepts children. When children are present we render
    // them verbatim (the dashboard uses this to render a "card-shaped"
    // copy target that looks like a normal status tile). Without children
    // we fall back to a labelled icon + text pill.
    if (children !== undefined) {
      return (
        <button
          type="button"
          className={classes}
          onClick={onCopy}
          title={title ?? "Copy"}
          aria-label={ariaLabel}
        >
          {children}
        </button>
      );
    }
    return (
      <button
        type="button"
        className={classes}
        onClick={onCopy}
        title={title ?? "Copy"}
        aria-label={ariaLabel}
      >
        <IconCopy />
        <span>{label ?? "Copy"}</span>
      </button>
    );
  }

  return (
    <button
      type="button"
      className={classes}
      onClick={onCopy}
      title={title ?? "Copy to clipboard"}
      aria-label={ariaLabel}
    >
      <IconCopy />
    </button>
  );
}
