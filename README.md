# adaptive-selector

**CSS selectors that survive website redesigns.**

Save an element's structural fingerprint once. When the site redesigns and your
selector breaks, relocate the element by similarity — tag, attributes, text,
path, parent, siblings — instead of waking up to an empty scrape.

```rust
use adaptive_selector::{AdaptiveDocument, SimilarityThreshold};

// Today: grab it with a normal selector
let doc = AdaptiveDocument::parse(r#"<span class="price">$9.99</span>"#);
let saved = doc.css_first(".price")?.expect("price exists").fingerprint();

// Tomorrow: the site redesigns, `.price` is gone
let doc2 = AdaptiveDocument::parse(r#"<span class="product-price__current">$9.99</span>"#);
let found = doc2.relocate(&saved, SimilarityThreshold::default());
assert_eq!(found[0].text(), "$9.99");
```

Fingerprints are plain JSON (`serde`) — persist them anywhere a scraper keeps
state, and relocate on any later parse of any later version of the page.

## What it is

A faithful Rust port of [Scrapling](https://github.com/D4Vinci/Scrapling)'s
**adaptive relocation engine** — the part that makes Scrapling's selectors
"learn" page changes — extracted into a dependency-light crate:

- `scraper` (html5ever) for parsing and CSS selection
- `serde` for the fingerprint
- no browser, no fetcher, no I/O — bring your own HTTP

If you want the full adaptive framework (stealth fetchers, spidering, Cloudflare
handling), use Scrapling — it's excellent. This crate is for Rust programs that
just want the *relocation* piece: the ~150-line similarity engine plus a clean
fingerprint/relocate API over `scraper`.

## The similarity score

Ported check-for-check from Scrapling's `__calculate_similarity_score`:

| signal | weight behavior |
|---|---|
| tag equality | 1 check, 0 or 1 |
| own text | difflib `SequenceMatcher.ratio()` |
| full attribute dict | keys-ratio × 0.5 + values-ratio × 0.5 |
| `class`, `id`, `href`, `src` individually | each a separate ratio check |
| root-to-element tag path | element-wise ratio over the tag sequence |
| parent name / attribs / text | three more checks when both have parents |
| sibling tag sequence | element-wise ratio over the sequence |

Score = (Σ signal scores / checks) × 100, rounded to 2 decimals. Default
acceptance threshold is **40** — Scrapling's own default, and the docs there
apply here: the number depends on page structure, don't tune it without cause.

## Digit-for-digit difflib

The engine leans on Python's `difflib.SequenceMatcher.ratio()`, which no Rust
similarity crate reproduces (`strsim`'s ratios differ on most inputs — it isn't
trying to be difflib). So this crate ships its own port of the
Ratcliff/Obershelp matching with difflib's "popular element" autojunk discount,
verified against CPython by a generated oracle:

```
tests/fixtures/difflib_oracle.json   # 30 (a, b, ratio) triples from live difflib
```

Every ratio this crate computes — strings, attribute dicts, tag paths — goes
through the same matcher. That means a fingerprint saved by this crate scores
the same way Scrapling's Python engine would score it.

## API

- `AdaptiveDocument::parse(&str)` — parse HTML
- `.css("sel")` / `.css_first("sel")` — normal selection, returns live elements
- `element.fingerprint()` — capture an `ElementFingerprint` (serializable)
- `doc.relocate(&fingerprint, threshold)` — best-scoring group above threshold
- `adaptive_selector::str_ratio(a, b)` — the difflib-compatible ratio, public in
  case you want to build your own scoring on the same substrate

## Install

```toml
adaptive-selector = "0.1"
```

## Status

0.1 — the engine, the oracle, and relocation tests over realistic redesign
fixtures. Deliberately not included yet (PRs welcome): XPath selection,
tied-score ranking heuristics beyond "all winners", async (it's CPU-bound over
a parsed tree — wrap it in `spawn_blocking` if you're in async land and the
document is large).

## Credits & license

The relocation algorithm and similarity scoring are ported from
[D4Vinci/Scrapling](https://github.com/D4Vinci/Scrapling) (BSD-3), with the
author's project being the reference implementation. The sequence matcher is
ported from CPython's `difflib` (PSF license). This crate is BSD-3-Clause.
