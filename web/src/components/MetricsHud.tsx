import { useSyncExternalStore } from "react";
import { HealthMonitor } from "./MainThreadHealth";
import type { FluxClient } from "flux-md";

interface Props {
  fluxClients: FluxClient[];
}

export function MetricsHud({ fluxClients }: Props) {
  const health = useSyncExternalStore(HealthMonitor.subscribe, HealthMonitor.getSnapshot, HealthMonitor.getSnapshot);

  let totalBytes = 0;
  let totalPatches = 0;
  let totalParseMs = 0;
  let totalRetained = 0;
  let totalWasmMem = 0;
  for (const c of fluxClients) {
    const m = c.getMetrics();
    totalBytes += m.bytes;
    totalPatches += m.patches;
    totalParseMs += m.totalParseMs;
    totalRetained += m.retainedBytes;
    totalWasmMem = Math.max(totalWasmMem, m.wasmMemoryBytes);
  }

  const bytes = totalBytes;
  const elapsedMs = fluxClients.reduce(
    (acc, c) => Math.max(acc, c.getMetrics().bytes > 0 ? performance.now() - (c as any).firstAppendMs : 0),
    1,
  );

  const kbps = (bytes / 1024) / Math.max(0.001, elapsedMs / 1000);

  const fpsColor = health.fps >= 55 ? "good" : health.fps >= 30 ? "warn" : "bad";
  const blockedColor = health.blockedMs < 100 ? "good" : health.blockedMs < 500 ? "warn" : "bad";

  return (
    <div className="flux-hud">
      <div className="flux-hud-row">
        <span className="flux-hud-label">FPS</span>
        <span className={`flux-hud-value flux-hud-${fpsColor}`}>{health.fps}</span>
      </div>
      <div className="flux-hud-row">
        <span className="flux-hud-label">main blocked</span>
        <span className={`flux-hud-value flux-hud-${blockedColor}`}>{health.blockedMs.toFixed(0)}ms</span>
      </div>
      <div className="flux-hud-row">
        <span className="flux-hud-label">throughput</span>
        <span className="flux-hud-value">{kbps.toFixed(1)} KB/s</span>
      </div>
      <div className="flux-hud-row">
        <span className="flux-hud-label">received</span>
        <span className="flux-hud-value">{(bytes / 1024).toFixed(1)} KB</span>
      </div>
      <div className="flux-hud-row">
        <span className="flux-hud-label">parse patches</span>
        <span className="flux-hud-value">{totalPatches}</span>
      </div>
      <div className="flux-hud-row">
        <span className="flux-hud-label">parse total</span>
        <span className="flux-hud-value">{totalParseMs.toFixed(1)}ms</span>
      </div>
      <div className="flux-hud-row">
        <span className="flux-hud-label">retained (src+html)</span>
        <span className="flux-hud-value">{(totalRetained / 1024).toFixed(1)} KB</span>
      </div>
      <div className="flux-hud-row">
        <span className="flux-hud-label">wasm heap (max)</span>
        <span className="flux-hud-value">{(totalWasmMem / 1024).toFixed(0)} KB</span>
      </div>
      <Sparkline values={health.blockedHistory} colorClass="bar-blocked" />
    </div>
  );
}

function Sparkline({ values, colorClass }: { values: number[]; colorClass: string }) {
  const max = Math.max(8, ...values);
  return (
    <div className="flux-spark">
      {values.map((v, i) => (
        <span
          key={i}
          className={`flux-spark-bar ${colorClass}`}
          style={{ height: `${Math.min(100, (v / max) * 100)}%` }}
        />
      ))}
    </div>
  );
}
