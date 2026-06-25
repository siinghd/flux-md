import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { createElement, act } from "react";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";
import { FluxClient, FluxPool } from "../src/client";
import { FluxMarkdown } from "../src/react";

// Synchronous fake worker (same shape as rerender-react.test.tsx).
class FakeWorker implements WorkerLike {
  sent: ToWorker[] = [];
  private listener: ((ev: { data: FromWorker }) => void) | null = null;
  postMessage(msg: ToWorker) {
    this.sent.push(msg);
  }
  addEventListener(_t: "message", l: (ev: { data: FromWorker }) => void) {
    this.listener = l;
  }
  terminate() {}
  fire(msg: FromWorker) {
    this.listener?.({ data: msg });
  }
}

let win: GlobalWindow;
beforeAll(() => {
  win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.window = win;
  g.navigator = win.navigator;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  g.Worker = class extends FakeWorker {} as unknown;
  (g as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

async function mount(node: ReturnType<typeof createElement>) {
  const { createRoot } = await import("react-dom/client");
  const host = win.document.createElement("div");
  const root = createRoot(host as unknown as Element);
  await act(async () => {
    root.render(node);
  });
  return { host, root };
}

function para(id: number, html: string, open: boolean): Block {
  return { id, kind: { type: "Paragraph" }, start: 0, end: 0, html, open, speculative: false };
}

const PATCH_META = { appendedBytes: 0, parseMicros: 0, retainedBytes: 0, wasmMemoryBytes: 0 } as const;

function newClient() {
  const w = new FakeWorker();
  const pool = new FluxPool(() => w, 1);
  const client = new FluxClient({ pool });
  client.append("");
  const sid = (w.sent[0] as { streamId: number }).streamId;
  return { w, client, sid };
}

// deferTail OFF (default): output is identical to the un-prop'd render and the
// root carries no `flux-deferred` class — the default path is unchanged.
test("deferTail off (default): output unchanged, no flux-deferred class", async () => {
  const { w, client, sid } = newClient();
  const { host } = await mount(createElement(FluxMarkdown, { client }));

  await act(async () => {
    w.fire({
      type: "patch",
      streamId: sid,
      patch: { newly_committed: [para(1, "<p>one</p>", false)], active: [para(2, "<p>tw</p>", true)] },
      ...PATCH_META,
    });
  });

  const root = host.firstElementChild!;
  expect(root.className).toBe("flux-md");
  expect(root.className).not.toContain("flux-deferred");
  expect(host.innerHTML).toContain("one");
  expect(host.innerHTML).toContain("tw");
});

// deferTail ON: renders without error and, on a single applied patch, is a
// no-op — same content, and once settled no `flux-deferred` class lingers.
test("deferTail on: renders without error, no-op on a single patch", async () => {
  const { w, client, sid } = newClient();
  const { host } = await mount(createElement(FluxMarkdown, { client, deferTail: true }));

  await act(async () => {
    w.fire({
      type: "patch",
      streamId: sid,
      patch: { newly_committed: [para(1, "<p>one</p>", false)], active: [para(2, "<p>two</p>", true)] },
      ...PATCH_META,
    });
  });

  const root = host.firstElementChild!;
  // Always carries the base class.
  expect(root.className).toContain("flux-md");
  // Content is rendered — deferTail never changes output, only commit timing.
  expect(host.innerHTML).toContain("one");
  expect(host.innerHTML).toContain("two");
  // After the act() flush the deferred value has caught up to the latest blocks,
  // so the transient `flux-deferred` marker is gone.
  expect(root.className).not.toContain("flux-deferred");
});

// deferTail ON preserves a caller className alongside flux-md.
test("deferTail on: caller className preserved", async () => {
  const { w, client, sid } = newClient();
  const { host } = await mount(createElement(FluxMarkdown, { client, deferTail: true, className: "mine" }));

  await act(async () => {
    w.fire({
      type: "patch",
      streamId: sid,
      patch: { newly_committed: [para(1, "<p>x</p>", false)], active: [] },
      ...PATCH_META,
    });
  });

  const root = host.firstElementChild!;
  expect(root.className).toContain("flux-md");
  expect(root.className).toContain("mine");
});
