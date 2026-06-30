// Framework-neutral URL + entity safety helpers, factored out of html-to-react
// so the DOM renderer (which must NOT import the React-coupled html-to-react)
// can share the EXACT same `safeUrl` scheme filter. Pure string logic only — no
// browser globals — so it is SSR-cold-import safe.

const NAMED_ENTITIES: Record<string, string> = {
  amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " ",
  copy: "©", reg: "®", hellip: "…", mdash: "—", ndash: "–",
};

/** Decode the (small, known) set of entities the core emits, plus numeric refs. */
export function decodeEntities(s: string): string {
  if (s.indexOf("&") === -1) return s;
  return s.replace(/&(#x[0-9a-fA-F]+|#\d+|[a-zA-Z][a-zA-Z0-9]*);/g, (m, body: string) => {
    if (body[0] === "#") {
      const code = body[1] === "x" || body[1] === "X"
        ? parseInt(body.slice(2), 16)
        : parseInt(body.slice(1), 10);
      if (Number.isNaN(code) || code < 0 || code > 0x10ffff) return m;
      try {
        return String.fromCodePoint(code);
      } catch {
        return m;
      }
    }
    const named = NAMED_ENTITIES[body];
    return named === undefined ? m : named;
  });
}

/** Replace a dangerous-scheme URL with "#". Mirrors the Rust `is_dangerous_scheme`:
 *  strip control chars (C0, DEL, C1 — matching Rust char::is_control),
 *  lowercase, then match. The strip affects only the probe, never output.
 *
 *  Exported as the SAFE URL path for user `decorators` / `urlTransform`: their
 *  output is a TRUSTED surface that does NOT pass through the attribute
 *  sanitizer, and React/the DOM happily render a `javascript:` href, so a
 *  decorator that builds a link must route its href through this (see
 *  `wrapLink`), and `urlTransform` output is re-run through it by the renderer. */
export function safeUrl(value: string): string {
  // Decode-STABLE probe: a value can be entity-decoded more than once before it
  // reaches the DOM, so peel layers to a fixpoint before the scheme check —
  // catches `javascript&#58;` and double-encoded `javascript&amp;#58;`. Only the
  // probe is decoded; the returned value is untouched (safe URLs stay verbatim).
  // Cap at 8 iterations: far beyond any legit URL (browsers entity-decode an
  // href once), and bounds the loop so a hostile value can't make it quadratic.
  let decoded = value;
  for (let i = 0, prev = ""; i < 8 && decoded !== prev; i++) {
    prev = decoded;
    decoded = decodeEntities(decoded);
  }
  // eslint-disable-next-line no-control-regex
  const probe = decoded.replace(/[\u0000-\u001f\u007f-\u009f]/g, "").replace(/^\s+/, "").toLowerCase();
  if (
    probe.startsWith("javascript:") ||
    probe.startsWith("vbscript:") ||
    probe.startsWith("data:text/html") ||
    probe.startsWith("data:text/javascript")
  ) {
    return "#";
  }
  return value;
}
