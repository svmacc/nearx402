#!/usr/bin/env python3
"""Render the public docs to dashboard/docs/ as static HTML, in the dashboard's own look.

No dependencies: a small Markdown subset that covers what the docs use (headings, paragraphs,
bullet and numbered lists with one level of nesting, task items, tables, fenced code, block
quotes, bold, italic, inline code and links). The page style is read from dashboard/index.html
at build time, so the docs always match the dashboard. Links between .md files become links
between the generated pages; links to anything else in the repository go to GitHub.

Usage, from the repository root:   python3 scripts/build-docs.py
Writes dashboard/docs/*.html and prints what it wrote. Safe to run again.
"""
import html
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "dashboard" / "docs"

# Order is the sidebar order. The README is the overview.
DOCS = [
    ("README.md", "index", "Overview"),
    ("docs/zk-policy.md", "zk-policy", "ZK policy design"),
    ("docs/threat-model.md", "threat-model", "Threat model"),
    ("docs/security-decisions.md", "security-decisions", "Security decisions"),
    ("docs/testnet.md", "testnet", "Testnet record"),
    ("docs/demo-script.md", "demo-script", "Demo script"),
    ("docs/reserved-recovery.md", "reserved-recovery", "Recovering a lost callback"),
    ("docs/zk-spike.md", "zk-spike", "Groth16 verifier spike"),
]
SLUG_BY_SRC = {src: slug for src, slug, _ in DOCS}


def repo_url():
    cfg = json.loads((ROOT / "dashboard" / "config.json").read_text(encoding="utf-8"))
    return cfg.get("repo", "").rstrip("/")


REPO = repo_url()


def slugify(text):
    """GitHub-style heading ids, so the docs' own #anchors keep working."""
    text = re.sub(r"`", "", text).strip().lower()
    text = re.sub(r"[^\w\s-]", "", text)
    return re.sub(r"\s+", "-", text)


def rewrite_link(url, src):
    """Map a markdown link target to its place on the site."""
    if re.match(r"^(https?:|mailto:|#)", url):
        return url
    target, _, anchor = url.partition("#")
    src_dir = Path(src).parent
    rel = (src_dir / target).as_posix() if target else src
    rel = re.sub(r"^(\./)+", "", rel)
    while rel.startswith("../"):
        rel = rel[3:]
    if rel in SLUG_BY_SRC:
        return SLUG_BY_SRC[rel] + ".html" + ("#" + anchor if anchor else "")
    if REPO:
        return f"{REPO}/blob/main/{rel}" + ("#" + anchor if anchor else "")
    return url


INLINE_LINK = re.compile(r"\[([^\]]+)\]\(([^)\s]+)\)")


def inline(text, src):
    """Inline markdown on one piece of text. Code spans are stashed first so that bold,
    italic and links can span across them (a bold phrase may contain a code span)."""
    codes = []

    def stash(m):
        codes.append("<code>" + html.escape(m.group(1)) + "</code>")
        return "\x00%d\x00" % (len(codes) - 1)

    s = re.sub(r"`([^`]+)`", stash, text)
    s = html.escape(s, quote=False)
    s = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", s)
    s = re.sub(r"(?<![\w*])\*(?!\s)(.+?)(?<!\s)\*(?![\w*])", r"<em>\1</em>", s)
    s = INLINE_LINK.sub(lambda m: '<a href="%s">%s</a>' % (html.escape(rewrite_link(m.group(2), src), quote=True), m.group(1)), s)
    return re.sub(r"\x00(\d+)\x00", lambda m: codes[int(m.group(1))], s)


def table(lines, src):
    rows = []
    for ln in lines:
        cells = [c.strip() for c in re.split(r"(?<!\\)\|", ln.strip().strip("|"))]
        rows.append([c.replace("\\|", "|") for c in cells])
    if len(rows) < 2 or not re.match(r"^\s*:?-{2,}", rows[1][0]):
        return "<p>" + "<br>".join(inline(l, src) for l in lines) + "</p>"
    head, body = rows[0], rows[2:]
    h = "<table><thead><tr>" + "".join("<th>%s</th>" % inline(c, src) for c in head) + "</tr></thead><tbody>"
    for r in body:
        r = r + [""] * (len(head) - len(r))
        h += "<tr>" + "".join("<td>%s</td>" % inline(c, src) for c in r[: len(head)]) + "</tr>"
    return h + "</tbody></table>"


LIST_ITEM = re.compile(r"^(\s*)([-*]|\d+[.)])\s+(.*)$")
TASK = re.compile(r"^\[([ xX])\]\s+(.*)$")


def render_list(items, src):
    """items: list of (indent, marker, text). One level of nesting by indentation."""
    def tag(marker):
        return "ol" if marker[0].isdigit() else "ul"
    out = []
    base = items[0][0]
    out.append("<%s>" % tag(items[0][1]))
    i = 0
    while i < len(items):
        ind, marker, text = items[i]
        sub = []
        j = i + 1
        while j < len(items) and items[j][0] > base:
            sub.append(items[j]); j += 1
        m = TASK.match(text)
        if m:
            box = '<span class="task done">&#10003;</span>' if m.group(1) != " " else '<span class="task">&nbsp;</span>'
            body = box + inline(m.group(2), src)
        else:
            body = inline(text, src)
        out.append("<li>" + body + (render_list(sub, src) if sub else "") + "</li>")
        i = j
    out.append("</%s>" % tag(items[0][1]))
    return "".join(out)


def convert(md, src):
    lines = md.splitlines()
    out, i, n = [], 0, len(lines)
    headings = []
    while i < n:
        ln = lines[i]
        if ln.startswith("```"):
            lang = ln[3:].strip()
            j = i + 1
            while j < n and not lines[j].startswith("```"):
                j += 1
            code = html.escape("\n".join(lines[i + 1 : j]))
            cls = ' class="lang-%s"' % html.escape(lang) if lang else ""
            note = '<div class="sub">Mermaid source; rendered on GitHub.</div>' if lang == "mermaid" else ""
            out.append("<pre%s><code>%s</code></pre>%s" % (cls, code, note))
            i = j + 1
            continue
        m = re.match(r"^(#{1,6})\s+(.*)$", ln)
        if m:
            level, text = len(m.group(1)), m.group(2).strip()
            sid = slugify(text)
            if level >= 2:
                headings.append((level, sid, re.sub(r"`", "", text)))
            out.append('<h%d id="%s">%s</h%d>' % (level, sid, inline(text, src), level))
            i += 1
            continue
        if ln.lstrip().startswith("|"):
            j = i
            while j < n and lines[j].lstrip().startswith("|"):
                j += 1
            out.append(table(lines[i:j], src))
            i = j
            continue
        if LIST_ITEM.match(ln):
            items = []
            j = i
            while j < n:
                lm = LIST_ITEM.match(lines[j])
                if lm:
                    items.append((len(lm.group(1)), lm.group(2), lm.group(3)))
                    j += 1
                elif lines[j].strip() and lines[j].startswith("  ") and items:
                    ind, mk, tx = items[-1]
                    items[-1] = (ind, mk, tx + " " + lines[j].strip())
                    j += 1
                else:
                    break
            out.append(render_list(items, src))
            i = j
            continue
        if ln.startswith(">"):
            j = i
            quote = []
            while j < n and lines[j].startswith(">"):
                quote.append(lines[j][1:].strip()); j += 1
            inner = convert("\n".join(quote), src)[0]
            out.append("<blockquote>%s</blockquote>" % inner)
            i = j
            continue
        if not ln.strip():
            i += 1
            continue
        j = i
        para = []
        while j < n and lines[j].strip() and not re.match(r"^(#{1,6}\s|```|\||>)", lines[j].lstrip()) and not LIST_ITEM.match(lines[j]):
            para.append(lines[j].strip()); j += 1
        out.append("<p>%s</p>" % inline(" ".join(para), src))
        i = j
    return "\n".join(out), headings


DOC_CSS = """
  /* docs pages */
  .docs { display: grid; grid-template-columns: 240px minmax(0, 1fr); gap: 32px; padding: 24px 32px 48px; }
  .docs aside { position: sticky; top: 16px; align-self: start; }
  .docs aside .k { color: var(--muted); font-size: 12.5px; font-weight: 500; letter-spacing: .04em; text-transform: uppercase; margin: 0 0 8px; }
  .docs aside a { display: block; color: var(--text); padding: 6px 10px; border-radius: 8px; font-size: 14px; }
  .docs aside a:hover { background: var(--panel-2); text-decoration: none; }
  .docs aside a[aria-current="page"] { background: var(--btn); color: var(--btn-text); }
  .docs aside .toc { margin-top: 18px; border-top: 1px solid var(--line); padding-top: 12px; }
  .docs aside .toc a { color: var(--muted); font-size: 13px; padding: 3px 10px; }
  .docs aside .toc a.l3 { padding-left: 22px; }
  article { max-width: 820px; min-width: 0; font-size: 15.5px; line-height: 1.6; }
  article h1 { font-size: 40px; margin: 0 0 18px; }
  article h2 { font-size: 24px; font-weight: 500; letter-spacing: -.02em; margin: 36px 0 10px; padding-top: 18px; border-top: 1px solid var(--line); }
  article h3 { font-size: 17px; font-weight: 600; margin: 24px 0 6px; }
  article p, article ul, article ol { margin: 0 0 12px; }
  article li { margin: 3px 0; }
  article ul ul, article ol ul, article ul ol { margin: 4px 0 0; }
  article pre { background: var(--panel-2); border: 1px solid var(--line); border-radius: 10px; padding: 12px 14px; overflow: auto; font-size: 12.5px; line-height: 1.5; margin: 0 0 12px; }
  article pre code { font-family: "JetBrains Mono", ui-monospace, Menlo, Consolas, monospace; }
  article code { font-family: "JetBrains Mono", ui-monospace, Menlo, Consolas, monospace; font-size: .9em; background: var(--panel-2); border: 1px solid var(--line); border-radius: 5px; padding: 0 4px; overflow-wrap: anywhere; }
  article pre code { background: none; border: 0; padding: 0; }
  article table { display: block; overflow-x: auto; margin: 0 0 14px; font-size: 14px; }
  article th, article td { padding: 8px 10px; }
  article blockquote { margin: 0 0 12px; padding: 4px 16px; border-left: 3px solid var(--accent); color: var(--muted); }
  article .task { display: inline-block; width: 15px; height: 15px; border: 1.5px solid var(--muted); border-radius: 4px; margin-right: 7px; vertical-align: -2px; font-size: 11px; line-height: 12px; text-align: center; color: var(--paid); }
  article .task.done { border-color: var(--paid); }
  .docs-source { margin-top: 32px; padding-top: 14px; border-top: 1px solid var(--line); }
  @media (max-width: 900px) {
    .docs { grid-template-columns: minmax(0, 1fr); padding-left: 16px; padding-right: 16px; }
    .docs aside { position: static; }
    .docs aside .toc { display: none; }
    article h1 { font-size: 30px; }
  }
"""


def page_style():
    src = (ROOT / "dashboard" / "index.html").read_text(encoding="utf-8")
    a = src.index("<style>") + len("<style>")
    b = src.index("</style>")
    return src[a:b] + DOC_CSS


def build():
    OUT.mkdir(parents=True, exist_ok=True)
    style = page_style()
    for src, slug, title in DOCS:
        md = (ROOT / src).read_text(encoding="utf-8")
        body, headings = convert(md, src)
        side = "".join(
            '<a href="%s.html"%s>%s</a>' % (s, ' aria-current="page"' if s == slug else "", t) for _, s, t in DOCS
        )
        toc = "".join('<a class="l%d" href="#%s">%s</a>' % (lvl, sid, html.escape(txt)) for lvl, sid, txt in headings if lvl <= 3)
        src_link = f'<a href="{REPO}/blob/main/{src}">{src}</a>' if REPO else src
        doc = f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{html.escape(title)} · nearx402 docs</title>
<meta name="description" content="nearx402 documentation: {html.escape(title)}. Rendered from the repository's markdown.">
<link rel="icon" href="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E%3Crect width='32' height='32' rx='7' fill='%23f96736'/%3E%3Ctext x='16' y='22' font-family='monospace' font-size='15' font-weight='700' fill='%23110c08' text-anchor='middle'%3E402%3C/text%3E%3C/svg%3E">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Figtree:ital,wght@0,400;0,500;0,600;0,700;1,700&family=JetBrains+Mono:wght@400;600&display=swap" rel="stylesheet">
<style>{style}</style>
</head>
<body>
<div class="frame">
<header>
  <nav>
    <div class="wordmark"><a href="../" style="color:inherit">near<em>x402</em></a><small>docs</small></div>
    <div class="status"><a href="../">Dashboard</a>{(' · <a href="' + REPO + '">GitHub</a>') if REPO else ''}</div>
  </nav>
</header>
<main class="docs">
  <aside>
    <div class="k">Documentation</div>
    {side}
    <div class="toc">{toc}</div>
  </aside>
  <article>
{body}
    <p class="sub docs-source">Rendered from {src_link} in the repository by <code>scripts/build-docs.py</code>. The markdown is the source of truth.</p>
  </article>
</main>
<footer><span>nearx402 · built at NEAR HACKATHON 2026</span><span class="wordmark">svm<em>tech</em></span></footer>
</div>
</body>
</html>
"""
        (OUT / f"{slug}.html").write_text(doc, encoding="utf-8", newline="\n")
        print(f"wrote dashboard/docs/{slug}.html  ({len(body):,} chars, {len(headings)} headings)")


if __name__ == "__main__":
    sys.exit(build())
