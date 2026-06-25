//! Math support: inline `$…$` / `\(…\)` and display `$$…$$` / `\[…\]`.
//! Covers the LaTeX `\(…\)` / `\[…\]` delimiters, the pandoc currency-safety
//! rule, HTML-escaping of the LaTeX body, streaming convergence, and
//! composition with lists / quotes / links / code spans.

use flux_md_core::StreamParser;

/// One-shot render with math on.
fn render(md: &str) -> String {
    let mut p = StreamParser::new().with_gfm_math(true);
    p.append(md);
    p.finalize();
    collect(&p)
}

/// Render fed one character at a time (the streaming path), math on.
fn render_streamed(md: &str) -> String {
    let mut p = StreamParser::new().with_gfm_math(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

/// One-shot render with math OFF (the default) — `$` must stay literal.
fn render_no_math(md: &str) -> String {
    let mut p = StreamParser::new();
    p.append(md);
    p.finalize();
    collect(&p)
}

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

// --------------------------------------------------------------------------
// Inline math
// --------------------------------------------------------------------------

#[test]
fn inline_dollar_math() {
    assert_eq!(render("$x$\n"), "<p><span class=\"math math-inline\">x</span></p>");
    assert!(render("the value $a^2 + b^2$ here\n").contains(
        "<span class=\"math math-inline\">a^2 + b^2</span>"
    ));
}

#[test]
fn inline_latex_paren_delim() {
    // #522: `\(…\)` is inline math, not literal escaped parens.
    assert_eq!(render("\\(x\\)\n"), "<p><span class=\"math math-inline\">x</span></p>");
    assert!(render("so \\(E = mc^2\\) holds\n").contains(
        "<span class=\"math math-inline\">E = mc^2</span>"
    ));
}

#[test]
fn inline_display_dollar_stays_inline() {
    // `$$x$$` with trailing prose is NOT a standalone block — render inline
    // (display) within the paragraph.
    let out = render("$$x$$ and more text\n");
    assert!(out.contains("<span class=\"math math-display\">x</span>"), "{out}");
    assert!(out.contains("and more text"), "{out}");
    assert!(out.starts_with("<p>") && out.ends_with("</p>"), "{out}");
}

#[test]
fn math_mid_paragraph_is_pure_inline() {
    // Math that is NOT at a line start never reaches the block scanner — it
    // goes straight through the inline parser. Both `$$…$$` (display) and
    // `\(…\)` (inline) must render in place inside the surrounding paragraph.
    let dollars = render("the identity $$e^{i\\pi} = -1$$ is famous\n");
    assert!(dollars.contains("the identity <span class=\"math math-display\">e^{i\\pi} = -1</span> is famous"), "{dollars}");
    assert!(dollars.starts_with("<p>") && dollars.ends_with("</p>"), "{dollars}");

    let paren = render("recall \\(a + b\\) before\n");
    assert!(paren.contains("recall <span class=\"math math-inline\">a + b</span> before"), "{paren}");
}

#[test]
fn math_body_is_html_escaped_not_markdown() {
    // `<`, `&`, `"` escaped; `*emph*` and `_x_` inside math are NOT processed.
    let out = render("$a < b & *c*$\n");
    assert!(out.contains("a &lt; b &amp; *c*"), "{out}");
    assert!(!out.contains("<em>"), "math body must not be markdown-processed: {out}");
}

// --------------------------------------------------------------------------
// Currency / pandoc disambiguation — must stay literal
// --------------------------------------------------------------------------

#[test]
fn currency_is_not_math() {
    // Pandoc rule: closer needs a non-space to its left, so `$5 and $10` is text.
    let out = render("I have $5 and $10 left.\n");
    assert!(!out.contains("class=\"math"), "currency became math: {out}");
    assert_eq!(out, "<p>I have $5 and $10 left.</p>");
}

#[test]
fn lone_dollar_and_open_only_stay_literal() {
    assert_eq!(render("costs $5 today\n"), "<p>costs $5 today</p>");
    // Opener immediately followed by space is rejected (pandoc).
    assert_eq!(render("a $ x$ b\n"), "<p>a $ x$ b</p>");
    // Closer followed by a digit is rejected.
    assert_eq!(render("$x$5\n"), "<p>$x$5</p>");
}

#[test]
fn escaped_dollar_is_literal() {
    let out = render("price \\$5 and \\$9\n");
    assert!(!out.contains("class=\"math"), "{out}");
    assert!(out.contains("price $5 and $9"), "{out}");
}

// --------------------------------------------------------------------------
// Block (display) math
// --------------------------------------------------------------------------

#[test]
fn block_dollar_single_line() {
    assert_eq!(render("$$x$$\n"), "<div class=\"math math-display\">x</div>");
}

#[test]
fn block_dollar_multiline_blank_tolerant() {
    // A blank line inside the fence must NOT split it (like a code fence).
    let out = render("$$\n\\begin{aligned}\na &= b \\\\\n\nc &= d\n\\end{aligned}\n$$\n");
    assert!(out.starts_with("<div class=\"math math-display\">"), "{out}");
    assert!(out.contains("\\begin{aligned}"), "{out}");
    assert!(out.contains("c &amp;= d"), "{out}");
    assert_eq!(out.matches("<div class=\"math math-display\">").count(), 1, "one block: {out}");
}

#[test]
fn block_latex_bracket_delim() {
    // #522: `\[…\]` display block.
    assert_eq!(render("\\[x\\]\n"), "<div class=\"math math-display\">x</div>");
    let out = render("\\[\nE = mc^2\n\\]\n");
    assert!(out.contains("<div class=\"math math-display\">E = mc^2</div>"), "{out}");
}

#[test]
fn block_math_interrupts_paragraph() {
    // `text` then a `$$` line with no blank between → paragraph then math block.
    let out = render("Here:\n$$\nx = 1\n$$\n");
    assert!(out.contains("<p>Here:</p>"), "{out}");
    assert!(out.contains("<div class=\"math math-display\">x = 1</div>"), "{out}");
}

// --------------------------------------------------------------------------
// Composition with other constructs
// --------------------------------------------------------------------------

#[test]
fn code_span_wins_over_math() {
    let out = render("`$x$` literally\n");
    assert!(out.contains("<code>$x$</code>"), "{out}");
    assert!(!out.contains("class=\"math"), "{out}");
}

#[test]
fn math_in_list_item() {
    let out = render("- $x$\n- plain\n");
    assert!(out.starts_with("<ul>"), "{out}");
    assert!(out.contains("<li><span class=\"math math-inline\">x</span></li>"), "{out}");
}

#[test]
fn math_in_blockquote() {
    let out = render("> $x$\n");
    assert!(out.contains("<blockquote>"), "{out}");
    assert!(out.contains("<span class=\"math math-inline\">x</span>"), "{out}");
}

#[test]
fn math_in_link_text() {
    let out = render("[$x$](http://example.com)\n");
    assert!(out.contains("href=\"http://example.com\""), "{out}");
    assert!(out.contains("<span class=\"math math-inline\">x</span>"), "{out}");
}

// --------------------------------------------------------------------------
// Off by default
// --------------------------------------------------------------------------

#[test]
fn math_off_keeps_dollars_literal() {
    assert_eq!(render_no_math("$x$\n"), "<p>$x$</p>");
    assert_eq!(render_no_math("$$x$$\n"), "<p>$$x$$</p>");
    // `\(x\)` with math off is the plain backslash-escape behavior: `(x)`.
    assert_eq!(render_no_math("\\(x\\)\n"), "<p>(x)</p>");
}

// --------------------------------------------------------------------------
// Streaming convergence + graceful degradation
// --------------------------------------------------------------------------

#[test]
fn streaming_matches_one_shot() {
    for md in [
        "$x$\n",
        "the value $a + b$ is here\n",
        "\\(x + y\\)\n",
        "$$x$$\n",
        "$$\nE = mc^2\n$$\n",
        "\\[\na = b\n\\]\n",
        "Here:\n$$\nx = 1\n$$\n",
        "- $x$\n- $y$\n",
        "I have $5 and $10 left.\n",
        "text with $a$ and then\n\nmore text $b$ after\n",
    ] {
        assert_eq!(render_streamed(md), render(md), "stream≠oneshot for {md:?}");
    }
}

#[test]
fn partial_math_degrades_gracefully() {
    // An open `$$` block (no closer yet) renders speculatively, never panics.
    let mut p = StreamParser::new().with_gfm_math(true);
    p.append("$$\nE = mc^2\n");
    let mid = collect(&p);
    assert!(mid.contains("<div class=\"math math-display\">"), "open block should render: {mid}");
    assert!(mid.contains("E = mc^2"), "{mid}");
    p.append("$$\n");
    p.finalize();
    assert_eq!(collect(&p), "<div class=\"math math-display\">E = mc^2</div>");

    // Unclosed inline forms stay literal at FINALIZE; nothing crashes at any cut
    // point. (open_tail is forced false at finalize → speculation is dead.)
    for md in ["$x", "$$x", "\\(x", "\\[x", "a $ b $ c", "$$$", "$"] {
        let _ = render(md);
        let _ = render_streamed(md);
    }
    assert!(!render("$x\n").contains("class=\"math"), "unclosed inline must stay literal");

    // ...but while the SAME inline forms are still streaming (open tail, no
    // finalize), they render speculatively as the resolved `<span class="math
    // …">` instead of flashing the raw `$`/`\(` source. Mid-stream view = no
    // finalize.
    let open = |md: &str| {
        let mut p = StreamParser::new().with_gfm_math(true);
        p.append(md);
        collect(&p)
    };
    assert!(
        open("$x^2").contains("<span class=\"math math-inline\">x^2</span>"),
        "open `$x^2` should speculate an inline-math span: {}",
        open("$x^2")
    );
    assert!(
        open("text \\(a+b").contains("<span class=\"math math-inline\">a+b</span>"),
        "open `\\(a+b` should speculate an inline-math span: {}",
        open("text \\(a+b")
    );
    // Inline display `$$…$$` inside a paragraph (`x $$y`, not a LEADING `$$`
    // which would open a block-math `<div>`): the open tail speculates an inline
    // display span over the partial body...
    assert!(
        open("x $$y").contains("<span class=\"math math-display\">y</span>"),
        "open `x $$y` should speculate an inline display-math span: {}",
        open("x $$y")
    );
    // ...and finalizing that very prefix collapses to literal (speculation is
    // streaming-only), byte-identical to the one-shot literal oracle.
    assert_eq!(render_streamed("x $$y"), render("x $$y"), "finalize of `x $$y` must be literal-parity");
    assert!(!render("x $$y").contains("class=\"math"), "finalized `x $$y` must be literal");
}
