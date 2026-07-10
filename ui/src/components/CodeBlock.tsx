// Monospace, dark-elevated code block with optional title row and copy button.
//
// Used by the dashboard's "Connect your tools" recipes and the headers
// cheat-sheet. Long URLs wrap with `word-break: break-all` so they don't
// overflow on narrow viewports.

import type { ReactNode } from "react";
import { CopyButton } from "./CopyButton";

export type CodeLanguage = "sh" | "toml" | "json" | "env" | "text" | "python";

interface CodeBlockProps {
  code: string;
  /** Subtle background tint per language. Defaults to "text". */
  language?: CodeLanguage;
  /** Optional small label rendered above the code (e.g. "shell", "config.json"). */
  title?: ReactNode;
  /** Show the Copy button in the top-right. Defaults to true. */
  copy?: boolean;
  /** Override the success toast message. */
  copyLabel?: string;
  className?: string;
  /** Optional element rendered before the copy button (e.g. a tag). */
  corner?: ReactNode;
}

export function CodeBlock({
  code,
  language = "text",
  title,
  copy = true,
  copyLabel,
  className,
  corner,
}: CodeBlockProps) {
  const langClass = `code-block-${language}`;
  const classes = ["code-block", langClass, className ?? ""]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={classes}>
      {(title || copy || corner) && (
        <div className="code-block-head">
          <span className="code-block-title">{title ?? language.toUpperCase()}</span>
          <span className="grow" />
          {corner}
          {copy ? (
            <CopyButton
              text={code}
              size="sm"
              title="Copy code to clipboard"
              successMsg={copyLabel ?? "Copied"}
            />
          ) : null}
        </div>
      )}
      <pre className="code-block-body">
        <code>{code}</code>
      </pre>
    </div>
  );
}
