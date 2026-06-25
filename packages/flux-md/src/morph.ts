/**
 * Tiny, self-contained idiomorph-style DOM morph (zero npm dependency, to keep
 * the security/supply-chain posture). It mutates a live element's children to
 * match a desired tree **in place** instead of replacing them wholesale, so the
 * browser only repaints/relayouts the parts that actually changed and focus +
 * text-selection survive a streaming update.
 *
 * Scope: this is intentionally minimal — it handles the cases the streaming
 * open-block tail produces (text growth, appended trailing nodes, attribute
 * tweaks). It is OPT-IN (see `morphOpenBlocks` in dom.ts) and never runs on the
 * default path.
 *
 * Matching strategy (per child level):
 *  - Build an id-set over the *new* subtree so a node carrying an `id` that the
 *    incoming tree also has can be matched across reorders/insertions.
 *  - Walk old/new children with two cursors. When the heads are "soft-equal"
 *    (same nodeType, same tagName for elements) we morph them in place and
 *    recurse; otherwise we id-match, insert, or remove the minimum.
 *  - Mutate matched element attributes/text in place; append trailing new nodes;
 *    remove old nodes with no match.
 */

/** Parse an HTML string into a detached `<template>`'s content (a DocumentFragment). */
export function htmlToFragment(html: string): DocumentFragment {
  const tpl = document.createElement("template");
  tpl.innerHTML = html;
  return tpl.content;
}

interface SavedFocus {
  node: Element;
  start: number | null;
  end: number | null;
}

// Snapshot the active element + (for text inputs/areas) its selection range, so
// a morph that touches the focused node — or its ancestors — does not blur it.
function saveFocus(root: Node): SavedFocus | null {
  if (typeof document === "undefined") return null;
  const active = document.activeElement;
  if (!active || active === document.body) return null;
  if (!root.contains(active)) return null;
  let start: number | null = null;
  let end: number | null = null;
  const el = active as HTMLInputElement & HTMLTextAreaElement;
  try {
    if (typeof el.selectionStart === "number") {
      start = el.selectionStart;
      end = el.selectionEnd;
    }
  } catch {
    // Some input types throw on selectionStart access; ignore.
  }
  return { node: active, start, end };
}

function restoreFocus(saved: SavedFocus | null): void {
  if (!saved) return;
  const node = saved.node as HTMLElement & HTMLInputElement & HTMLTextAreaElement;
  // Still attached? (a morph could have removed it — then there's nothing to do)
  if (typeof document !== "undefined" && !document.contains(node)) return;
  if (document.activeElement === node) {
    // Selection may still need restoring even if focus was kept.
  } else if (typeof node.focus === "function") {
    try {
      node.focus({ preventScroll: true });
    } catch {
      try {
        node.focus();
      } catch {
        /* not focusable anymore */
      }
    }
  }
  if (saved.start !== null && typeof node.setSelectionRange === "function") {
    try {
      node.setSelectionRange(saved.start, saved.end ?? saved.start);
    } catch {
      // setSelectionRange throws for input types that don't support it.
    }
  }
}

/**
 * Morph `from`'s children to match `to` (a fragment, element, or HTML string).
 * `from` itself is never replaced — only its subtree is reconciled in place.
 */
export function morph(from: Element, to: string | DocumentFragment | Element): void {
  const fragment: DocumentFragment | Element =
    typeof to === "string" ? htmlToFragment(to) : to;
  const saved = saveFocus(from);
  morphChildren(from, fragment);
  restoreFocus(saved);
}

// Two element nodes can be morphed into one another (vs. replaced) when they are
// the same element type. Text/comment nodes match on nodeType alone.
function isSoftMatch(a: Node, b: Node): boolean {
  if (a.nodeType !== b.nodeType) return false;
  if (a.nodeType === 1) {
    return (a as Element).tagName === (b as Element).tagName;
  }
  return true;
}

function nodeId(n: Node): string | null {
  if (n.nodeType !== 1) return null;
  const id = (n as Element).getAttribute("id");
  return id && id.length > 0 ? id : null;
}

// Collect the set of element ids present anywhere in `parent`'s direct children
// (one level — recursion handles deeper levels). Used so a keyed node can be
// matched across insertions before we resort to soft-matching.
function childIdSet(parent: Node): Set<string> {
  const ids = new Set<string>();
  for (let c = parent.firstChild; c; c = c.nextSibling) {
    const id = nodeId(c);
    if (id) ids.add(id);
  }
  return ids;
}

function morphChildren(oldParent: Node, newParent: Node): void {
  const newIds = childIdSet(newParent);

  let oldChild = oldParent.firstChild;
  let newChild = newParent.firstChild;

  while (newChild) {
    const nextNew = newChild.nextSibling;

    // No more old nodes → append the rest of the new ones (the common streaming
    // case: trailing tokens/elements grew onto the tail). We must import nodes
    // out of the source fragment into the live document.
    if (!oldChild) {
      oldParent.appendChild(importNode(oldParent, newChild));
      newChild = nextNew;
      continue;
    }

    const nextOld = oldChild.nextSibling;

    // Exact id match at the head → morph in place.
    const newKey = nodeId(newChild);
    const oldKey = nodeId(oldChild);

    if (isSoftMatch(oldChild, newChild) && newKey === oldKey) {
      morphNode(oldChild, newChild);
      oldChild = nextOld;
      newChild = nextNew;
      continue;
    }

    // The old head carries an id the new tree no longer has → it's gone; remove
    // it and retry this new child against the next old node.
    if (oldKey && !newIds.has(oldKey)) {
      const toRemove = oldChild;
      oldChild = nextOld;
      oldParent.removeChild(toRemove);
      continue;
    }

    // The new head has an id that exists further down the old list → pull that
    // old node up to the head, morph, and continue.
    if (newKey) {
      const match = findChildById(oldParent, newKey);
      if (match) {
        if (match !== oldChild) oldParent.insertBefore(match, oldChild);
        morphNode(match, newChild);
        oldChild = match.nextSibling;
        newChild = nextNew;
        continue;
      }
      // New keyed node not present in the old tree → insert a fresh import.
      oldParent.insertBefore(importNode(oldParent, newChild), oldChild);
      newChild = nextNew;
      continue;
    }

    // Soft (unkeyed) match → morph in place.
    if (isSoftMatch(oldChild, newChild)) {
      morphNode(oldChild, newChild);
      oldChild = nextOld;
      newChild = nextNew;
      continue;
    }

    // No match at all: replace the old head with an import of the new head.
    oldParent.insertBefore(importNode(oldParent, newChild), oldChild);
    oldParent.removeChild(oldChild);
    oldChild = nextOld;
    newChild = nextNew;
  }

  // Any leftover old children have no counterpart → remove them.
  while (oldChild) {
    const next = oldChild.nextSibling;
    oldParent.removeChild(oldChild);
    oldChild = next;
  }
}

// Find a direct child of `parent` whose element id equals `id`.
function findChildById(parent: Node, id: string): Node | null {
  for (let c = parent.firstChild; c; c = c.nextSibling) {
    if (nodeId(c) === id) return c;
  }
  return null;
}

// Bring a node from a source fragment into the live document so it can be
// inserted. `importNode(deep)` clones; the source fragment is discarded after.
function importNode(target: Node, source: Node): Node {
  const doc = target.ownerDocument ?? document;
  return doc.importNode(source, true);
}

// Morph a single matched node `oldNode` toward `newNode`. Elements get their
// attributes synced and children recursed; text/comment nodes get their data
// updated in place (so the live text node identity — and any caret in it —
// survives a token append).
function morphNode(oldNode: Node, newNode: Node): void {
  if (oldNode.nodeType === 1) {
    morphAttributes(oldNode as Element, newNode as Element);
    morphChildren(oldNode, newNode);
    return;
  }
  // Text (3) / comment (8) / CDATA: update character data in place.
  if (oldNode.nodeValue !== newNode.nodeValue) {
    oldNode.nodeValue = newNode.nodeValue;
  }
}

function morphAttributes(oldEl: Element, newEl: Element): void {
  // Set/update attributes present on the new element.
  const newAttrs = newEl.attributes;
  for (let i = 0; i < newAttrs.length; i++) {
    const attr = newAttrs[i];
    if (oldEl.getAttribute(attr.name) !== attr.value) {
      oldEl.setAttribute(attr.name, attr.value);
    }
  }
  // Remove attributes the new element no longer has. Iterate over a snapshot of
  // names because removeAttribute mutates the live NamedNodeMap.
  const oldAttrs = oldEl.attributes;
  const stale: string[] = [];
  for (let i = 0; i < oldAttrs.length; i++) {
    const name = oldAttrs[i].name;
    if (!newEl.hasAttribute(name)) stale.push(name);
  }
  for (const name of stale) oldEl.removeAttribute(name);
}
