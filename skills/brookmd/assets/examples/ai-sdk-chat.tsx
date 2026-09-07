/**
 * Vercel AI SDK `useChat` → brookmd.
 *
 * `useChat` hands you a growing STRING per message (joined from `message.parts`),
 * not a stream — so bridge it with `useBrookMarkdownString`, which diffs the
 * string for you: a prefix-extension appends only the delta, a divergence
 * resets and reparses.
 *
 * Three non-obvious points, all load-bearing:
 *   1. Pass `streaming: false` when the message is finished. brookmd never
 *      infers "done" from an absent flag (that would re-finalize on every token
 *      — an O(n^2) reparse trap), so without it the last block stays open
 *      forever: a finished fence never highlights and never shows its Copy button.
 *   2. HOIST the `components` map, `decorators`, `urlTransform` and `sanitize`.
 *      A fresh object identity each render busts the per-block memo and
 *      re-renders the whole document on every patch.
 *   3. `getDefaultPool().warm()` in the chat shell moves WASM init off the
 *      first-token critical path.
 */
import { memo, useEffect } from "react";
import { useChat } from "@ai-sdk/react";
import { BrookMarkdown, useBrookMarkdownString } from "brookmd/react";
import { getDefaultPool } from "brookmd/client";
import type { Components, ParserConfig } from "brookmd";

// Hoisted — module scope, one stable identity for the life of the app.
const CHAT_CONFIG: ParserConfig = {
  softBreaks: true, // models emit single newlines and expect a visible break
  dirAuto: true, // per-block bidi for multilingual answers
  a11y: true, // <label>-wrapped task checkboxes, scope="col" headers
  blockData: true, // typed table/heading/code/math data for toolbars
  gfmMath: true, // $…$ / $$…$$ → KaTeX-ready .math markup
};

const COMPONENTS: Components = {
  a: (props: { href?: string; children?: unknown }) => (
    <a href={props.href} className="chat-link">
      {props.children as never}
    </a>
  ),
};

/** One assistant/user bubble. Memoized so a sibling's patch never re-renders it. */
const Message = memo(function Message({
  text,
  streaming,
}: {
  text: string;
  streaming: boolean;
}) {
  const client = useBrookMarkdownString(text, { config: CHAT_CONFIG, streaming });
  return (
    <BrookMarkdown
      client={client}
      components={COMPONENTS}
      className="brook-caret"
      stickToBottom
      role="log"
      aria-live="polite"
    />
  );
});

type UiMessage = { id: string; role: string; parts?: { type: string; text?: string }[] };

/** Join every text part of a message into the document-so-far. */
function messageText(message: UiMessage): string {
  return (message.parts ?? [])
    .filter((p) => p.type === "text")
    .map((p) => p.text ?? "")
    .join("");
}

export function Chat() {
  const { messages, status } = useChat() as {
    messages: UiMessage[];
    status: string;
  };

  useEffect(() => {
    getDefaultPool().warm();
  }, []);

  const lastId = messages.length > 0 ? messages[messages.length - 1]!.id : null;

  return (
    <div className="chat" style={{ overflowY: "auto", scrollSnapType: "y proximity" }}>
      {messages.map((m) => (
        <Message
          key={m.id}
          text={messageText(m)}
          // Only the LAST message can still be streaming.
          streaming={status === "streaming" && m.id === lastId}
        />
      ))}
    </div>
  );
}
