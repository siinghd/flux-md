import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { FluxClient, FluxMarkdown, getDefaultPool } from "flux-md";
import { ComponentsDemo } from "./ComponentsDemo";
import { MetricsHud } from "./MetricsHud";
import { HealthMonitor } from "./MainThreadHealth";
import { streamChat, type ChatMessage } from "../streaming/openai";

const NUM_STREAMS = 5;

// Opt into content-visibility block virtualization via ?virtualize=1 (off by
// default, matching the library default).
const VIRTUALIZE =
  typeof window !== "undefined" && new URLSearchParams(window.location.search).has("virtualize");

// Opt into CSS-only stick-to-bottom via ?stick=1: flux-md emits a snap target,
// and we put scroll-snap-type on the flux pane's scroller (see .lab-stick CSS).
const STICK =
  typeof window !== "undefined" && new URLSearchParams(window.location.search).has("stick");

const DEFAULT_PROMPT = `Write a 1500-word technical deep-dive on building a high-performance markdown streaming parser. Include:

1. Several level-2 and level-3 headings.
2. An ordered list of design constraints, and an unordered list of trade-offs.
3. Bold and italic emphasis throughout.
4. At least three fenced code blocks: one in Rust (a state machine), one in TypeScript (a web worker), and one in Bash (build commands).
5. A markdown table comparing four parsing approaches across speed, memory, and correctness.
6. A blockquote with a memorable insight.
7. Inline \`code\` references to specific functions like \`appendChunk\`, \`reparseTail\`, and \`commitBlock\`.
8. Links to fictional documentation pages.
9. A mermaid diagram: \`\`\`mermaid\\ngraph TD\\nA[Input]-->B[Worker]\\nB-->C[Parser]\\nC-->D[Render]\\n\`\`\`
10. A LaTeX math expression in a \`\`\`math fenced block.

Make it dense, technically substantive, and well-organized. Aim for at least 1500 words.`;

interface StreamState {
  text: string;
  client: FluxClient;
  done: boolean;
  startMs: number;
  endMs: number;
  abort: AbortController | null;
}

function newStreamState(): StreamState {
  return {
    text: "",
    client: new FluxClient(),
    done: true,
    startMs: 0,
    endMs: 0,
    abort: null,
  };
}

export function StreamLab() {
  const [prompt, setPrompt] = useState(DEFAULT_PROMPT);
  const [running, setRunning] = useState(false);
  const [, force] = useState(0);
  const streamsRef = useRef<StreamState[]>([]);
  if (streamsRef.current.length === 0) {
    streamsRef.current = Array.from({ length: NUM_STREAMS }, newStreamState);
  }

  const start = useCallback(async () => {
    if (running) return;
    setRunning(true);
    HealthMonitor.reset();

    // Reset every stream.
    for (let i = 0; i < NUM_STREAMS; i++) {
      const s = streamsRef.current[i];
      s.client.reset();
      s.text = "";
      s.done = false;
      s.startMs = performance.now();
      s.endMs = 0;
      s.abort = new AbortController();
    }
    force((x) => x + 1);

    await Promise.all(
      streamsRef.current.map(async (s, i) => {
        await s.client.whenReady();
        const messages: ChatMessage[] = [
          { role: "system", content: "You are a precise, prolific technical writer." },
          { role: "user", content: `${prompt}\n\n(This is stream #${i + 1}/${NUM_STREAMS}. Use a slightly different angle than the others.)` },
        ];
        await streamChat({
          messages,
          signal: s.abort?.signal,
          onChunk: (delta) => {
            s.text += delta;
            s.client.append(delta);
          },
          onDone: () => {
            s.done = true;
            s.endMs = performance.now();
            s.client.finalize();
            force((x) => x + 1);
          },
          onError: (err) => {
            s.done = true;
            s.endMs = performance.now();
            s.client.finalize();
            // eslint-disable-next-line no-console
            console.error("stream", i, "error:", err);
            force((x) => x + 1);
          },
        });
      }),
    );

    setRunning(false);
  }, [running, prompt]);

  const stop = useCallback(() => {
    for (const s of streamsRef.current) s.abort?.abort();
    setRunning(false);
  }, []);

  const reset = useCallback(() => {
    stop();
    for (let i = 0; i < NUM_STREAMS; i++) {
      const s = streamsRef.current[i];
      s.client.reset();
      s.text = "";
      s.done = true;
      s.startMs = 0;
      s.endMs = 0;
    }
    HealthMonitor.reset();
    force((x) => x + 1);
  }, [stop]);

  // Deterministic replay hook for headless tests. Plays a fixed markdown corpus
  // into all 5 streams at a fixed rate, no network.
  // Call from devtools: window.__fluxReplay("# Hello\n\nsome markdown...", 16)
  useEffect(() => {
    (window as any).__fluxReplay = async (corpus: string, chunkSize = 16, intervalMs = 1) => {
      // Reset everything first.
      for (let i = 0; i < NUM_STREAMS; i++) {
        const s = streamsRef.current[i];
        s.client.reset();
        s.text = "";
        s.done = false;
        s.startMs = performance.now();
        s.endMs = 0;
      }
      HealthMonitor.reset();
      force((x) => x + 1);
      await Promise.all(streamsRef.current.map((s) => s.client.whenReady()));

      // Walk the corpus, dispatching to all 5 streams in parallel.
      let pos = 0;
      while (pos < corpus.length) {
        const next = Math.min(pos + chunkSize, corpus.length);
        const chunk = corpus.slice(pos, next);
        for (let i = 0; i < NUM_STREAMS; i++) {
          const s = streamsRef.current[i];
          s.text += chunk;
          s.client.append(chunk);
        }
        pos = next;
        if (intervalMs > 0) {
          await new Promise((r) => setTimeout(r, intervalMs));
        }
      }
      for (const s of streamsRef.current) {
        s.done = true;
        s.endMs = performance.now();
        s.client.finalize();
      }
      force((x) => x + 1);
    };
    // Dev/test hook: spin up N clients on the shared pool (N > worker cap), feed
    // each a *unique* document, and verify every stream parsed correctly. This
    // forces multiple parsers onto one real worker — exercising the multiplexing
    // path the unit tests can only fake. Returns isolation + worker-count proof.
    (window as any).__fluxMultiplexCheck = async (n = 12) => {
      const clients = Array.from({ length: n }, () => new FluxClient());
      await Promise.all(clients.map((c) => c.whenReady()));
      for (let i = 0; i < n; i++) {
        clients[i].append(`# Doc ${i}\n\nUnique paragraph number ${i} here.\n`);
        clients[i].finalize();
      }
      await new Promise((r) => setTimeout(r, 400));
      let correct = 0;
      for (let i = 0; i < n; i++) {
        const html = clients[i].getSnapshot().map((b) => b.html).join("");
        // Correct + isolated: stream i shows ONLY its own doc, no cross-talk.
        if (html.includes(`Doc ${i}`) && html.includes(`number ${i} here`) && !html.includes(`Doc ${i + 1}`)) {
          correct++;
        }
      }
      const workerCount = getDefaultPool().workerCount;
      for (const c of clients) c.destroy();
      return { n, workerCount, correct, allCorrect: correct === n };
    };
    return () => {
      delete (window as any).__fluxReplay;
      delete (window as any).__fluxMultiplexCheck;
    };
  }, []);

  useEffect(() => () => stop(), [stop]);

  return (
    <div className={"lab" + (STICK ? " lab-stick" : "")}>
      <header className="lab-head">
        <div className="lab-brand">
          <span className="lab-bolt">⚡</span>
          <h1>flux-md</h1>
          <span className="lab-sub">streaming markdown that doesn't melt your tab</span>
        </div>
        <div className="lab-controls">
          {!running ? (
            <button className="lab-btn lab-btn-primary" onClick={start}>
              ▶ Run {NUM_STREAMS} concurrent streams
            </button>
          ) : (
            <button className="lab-btn lab-btn-danger" onClick={stop}>
              ■ Stop
            </button>
          )}
          <button className="lab-btn" onClick={reset} disabled={running}>
            Reset
          </button>
        </div>
      </header>

      <details className="lab-prompt-edit">
        <summary>Prompt (sent to <code>ai.hsingh.app/v1/chat/completions</code>)</summary>
        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          rows={8}
          disabled={running}
          spellCheck={false}
        />
      </details>

      <ComponentsDemo />

      <div className="lab-board lab-board-flux">
        <section className="lab-pane lab-pane-flux">
          <PaneHeader title="flux-md" subtitle={`${getDefaultPool().workerCount} pooled workers, incremental parse`}>
            <MetricsHud fluxClients={streamsRef.current.map((s) => s.client)} />
          </PaneHeader>
          <div className="lab-grid">
            {streamsRef.current.map((s, i) => (
              <div className="lab-cell" key={i}>
                <FluxCellHeader index={i} state={s} />
                <div className="lab-cell-body">
                  <FluxMarkdown client={s.client} virtualize={VIRTUALIZE} stickToBottom={STICK} />
                </div>
              </div>
            ))}
          </div>
        </section>
      </div>

      <footer className="lab-foot">
        <span>
          Zero-dep Rust→WASM core (<code>150 KB</code>) · Pooled workers ·
          <a className="lab-foot-badge" href="https://spec.commonmark.org/0.31/" target="_blank" rel="noopener noreferrer">
            CommonMark 0.31: <strong>100%</strong> (652/652)
          </a>{" "}
          · GFM tables/strikethrough/task-lists/autolinks/alerts/footnotes
        </span>
        <span>
          API: <code>ai.hsingh.app/v1/chat/completions</code> · model:{" "}
          <code>auto</code>
        </span>
      </footer>
    </div>
  );
}

function PaneHeader({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <div className="lab-pane-head">
      <div className="lab-pane-title">
        <strong>{title}</strong>
        <span className="lab-pane-sub">{subtitle}</span>
      </div>
      <div className="lab-pane-hud">{children}</div>
    </div>
  );
}

function FluxCellHeader({ index, state }: { index: number; state: StreamState }) {
  // Subscribe to this stream's client so per-stream parse / retained metrics
  // update live without a parent re-render.
  useSyncExternalStore(state.client.subscribe, state.client.getSnapshot, state.client.getSnapshot);
  const m = state.client.getMetrics();
  const dur = state.endMs > 0 ? state.endMs - state.startMs : state.startMs > 0 ? performance.now() - state.startMs : 0;
  return (
    <div className="lab-cell-head">
      <span className="lab-cell-num">#{index + 1}</span>
      <span className={"lab-cell-status " + (state.done ? "done" : "live")}>{state.done ? "done" : "streaming"}</span>
      <span className="lab-cell-dur">{(dur / 1000).toFixed(1)}s</span>
      <span className="lab-cell-bytes">{(state.text.length / 1024).toFixed(1)} KB</span>
      <span className="lab-cell-divider">·</span>
      <span className="lab-cell-met" title="cumulative parse time on the worker thread">
        {m.totalParseMs.toFixed(1)}ms parse
      </span>
      <span className="lab-cell-met" title="bytes retained by the Rust parser: source buffer + committed HTML">
        {(m.retainedBytes / 1024).toFixed(1)} KB held
      </span>
    </div>
  );
}
