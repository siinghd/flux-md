import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { FluxClient, FluxMarkdown } from "flux-md";
import { streamChat, type ChatMessage } from "../streaming/openai";

interface Turn {
  id: number;
  role: "user" | "assistant";
  /** User text, or the assistant's accumulated text (for API history). */
  text: string;
  /** Assistant only — owns the streaming parser/worker. */
  client?: FluxClient;
  done: boolean;
}

const SYSTEM_PROMPT =
  "You are a helpful assistant. Reply in clean GitHub-Flavored Markdown. Put a " +
  "blank line between every block — headings, paragraphs, lists, tables, and code " +
  "fences. Open a fenced code block on its own line as ```lang followed by a " +
  "newline, and close it with ``` on its own line. Use $…$ for inline math and " +
  "$$…$$ on their own lines for display math. Be substantive, not padded.";

const EXAMPLES = [
  "Explain how a streaming markdown parser stays O(n), with a Rust code sketch.",
  "Give me a comparison table of three rope data structures.",
  "Derive the quadratic formula with LaTeX, step by step.",
];

export function Chat() {
  const [turns, setTurns] = useState<Turn[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const nextId = useRef(0);
  const abortRef = useRef<AbortController | null>(null);
  const scrollerRef = useRef<HTMLDivElement>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);
  // Whether the view is pinned to the bottom (released when the user scrolls up).
  const stickRef = useRef(true);

  const scrollToBottom = useCallback(() => {
    const el = scrollerRef.current;
    if (el && stickRef.current) el.scrollTop = el.scrollHeight;
  }, []);

  // While streaming, the assistant block grows asynchronously (via the worker),
  // so poll-scroll to follow the tail until done.
  useEffect(() => {
    if (!busy) return;
    const t = setInterval(scrollToBottom, 80);
    return () => clearInterval(t);
  }, [busy, scrollToBottom]);

  useLayoutEffect(scrollToBottom, [turns.length, scrollToBottom]);

  // flux-md emits KaTeX-ready markup (`<span|div class="math …">LaTeX</span|div>`)
  // and stays zero-dep; the demo brings KaTeX (loaded from CDN in index.html) and
  // typesets each math element once its block has closed (open blocks hold partial
  // LaTeX, so we skip anything still inside a streaming block).
  useEffect(() => {
    const root = scrollerRef.current;
    if (!root) return;
    let retry = 0;
    const pass = () => {
      const katex = (window as unknown as { katex?: any }).katex;
      if (!katex) {
        retry = window.setTimeout(pass, 200);
        return;
      }
      root.querySelectorAll<HTMLElement>(".math:not([data-tex])").forEach((el) => {
        if (el.closest(".flux-streaming, .flux-open")) return; // still streaming
        el.setAttribute("data-tex", "1");
        try {
          katex.render(el.textContent ?? "", el, {
            displayMode: el.classList.contains("math-display"),
            throwOnError: false,
            output: "html",
          });
        } catch {
          /* leave the raw LaTeX in place */
        }
      });
    };
    const obs = new MutationObserver(() => pass());
    obs.observe(root, { childList: true, subtree: true });
    pass();
    return () => {
      obs.disconnect();
      if (retry) clearTimeout(retry);
    };
  }, []);

  const onScroll = useCallback(() => {
    const el = scrollerRef.current;
    if (!el) return;
    stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 96;
  }, []);

  const grow = useCallback(() => {
    const ta = taRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = Math.min(ta.scrollHeight, 200) + "px";
  }, []);

  const send = useCallback(
    async (raw?: string) => {
      const text = (raw ?? input).trim();
      if (!text || busy) return;
      setInput("");
      requestAnimationFrame(grow);
      stickRef.current = true;
      setBusy(true);

      const history: ChatMessage[] = [
        { role: "system", content: SYSTEM_PROMPT },
        ...turns.map((t) => ({ role: t.role, content: t.text })),
        { role: "user", content: text },
      ];

      const client = new FluxClient({ config: { gfmMath: true } });
      const userTurn: Turn = { id: nextId.current++, role: "user", text, done: true };
      const aiTurn: Turn = { id: nextId.current++, role: "assistant", text: "", client, done: false };
      setTurns((prev) => [...prev, userTurn, aiTurn]);

      const finish = () => {
        aiTurn.done = true;
        client.finalize();
        abortRef.current = null;
        setBusy(false);
        setTurns((prev) => [...prev]); // flip the streaming indicator
      };

      await client.whenReady();
      const abort = new AbortController();
      abortRef.current = abort;
      await streamChat({
        messages: history,
        signal: abort.signal,
        onChunk: (delta) => {
          aiTurn.text += delta;
          client.append(delta);
        },
        onDone: finish,
        onError: (err) => {
          if (!aiTurn.text) {
            client.append(`\n> **Stream error.** ${String(err.message ?? err)}\n`);
          }
          finish();
        },
      });
    },
    [input, busy, turns, grow],
  );

  const stop = useCallback(() => {
    abortRef.current?.abort();
  }, []);

  const reset = useCallback(() => {
    abortRef.current?.abort();
    for (const t of turns) t.client?.destroy();
    setTurns([]);
    setBusy(false);
    stickRef.current = true;
  }, [turns]);

  useEffect(() => {
    // Free every stream's worker-side parser when the chat unmounts.
    return () => {
      for (const t of turns) t.client?.destroy();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  };

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="bolt">⚡</span>
          <b>flux-md</b>
          <span className="brand-tag">streaming markdown</span>
        </div>
        {turns.length > 0 && (
          <button className="ghost-btn" onClick={reset} disabled={busy} title="New conversation">
            new chat
          </button>
        )}
      </header>

      <div className="scroller" ref={scrollerRef} onScroll={onScroll}>
        {turns.length === 0 ? (
          <div className="empty">
            <h1 className="empty-title">
              Markdown that renders <em>as it streams.</em>
            </h1>
            <p className="empty-sub">
              A live demo of <b>flux-md</b> — a zero-dep, Rust→WASM streaming parser. Ask
              anything; the reply parses incrementally, off the main thread.
            </p>
            <div className="chips">
              {EXAMPLES.map((ex) => (
                <button key={ex} className="chip" onClick={() => send(ex)}>
                  {ex}
                </button>
              ))}
            </div>
          </div>
        ) : (
          <div className="thread">
            {turns.map((t) =>
              t.role === "user" ? (
                <div className="msg msg-user" key={t.id}>
                  <div className="bubble">{t.text}</div>
                </div>
              ) : (
                <div className="msg msg-ai" key={t.id}>
                  <div className="ai-mark">⚡</div>
                  <div className="ai-body">
                    {t.client && <FluxMarkdown client={t.client} />}
                  </div>
                </div>
              ),
            )}
          </div>
        )}
      </div>

      <div className="dock">
        <div className="composer">
          <div className={"composer-card" + (busy ? " is-busy" : "")}>
            <textarea
              ref={taRef}
              className="composer-input"
              value={input}
              placeholder="Ask something…"
              rows={1}
              spellCheck={false}
              onChange={(e) => {
                setInput(e.target.value);
                grow();
              }}
              onKeyDown={onKeyDown}
            />
            {busy ? (
              <button className="send-btn is-stop" onClick={stop} aria-label="Stop" title="Stop">
                <span className="stop-glyph" />
              </button>
            ) : (
              <button
                className="send-btn"
                onClick={() => send()}
                disabled={!input.trim()}
                aria-label="Send"
                title="Send"
              >
                <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true">
                  <path
                    d="M12 19V5M12 5l-6 6M12 5l6 6"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2.2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  />
                </svg>
              </button>
            )}
          </div>
          <p className="hint">
            <kbd>Enter</kbd> to send · <kbd>Shift</kbd>+<kbd>Enter</kbd> for a newline
          </p>
        </div>
      </div>
    </div>
  );
}
