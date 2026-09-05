//! adaptive-selector — CSS selectors that survive website redesigns.
//!
//! A faithful Rust port of Scrapling's adaptive relocation engine. Save an
//! element's fingerprint once; when the page changes and your CSS selector
//! breaks, relocate the element by structural similarity instead of by
//! selector.
//!
//! ```no_run
//! use adaptive_selector::{AdaptiveDocument, SimilarityThreshold};
//!
//! let doc = AdaptiveDocument::parse(r#"<div class="price">$9.99</div>"#);
//! let saved = doc.css_first(".price").unwrap().expect("price exists").fingerprint();
//!
//! // ... the site redesigns, `.price` is now `.product-price__current` ...
//! let doc2 = AdaptiveDocument::parse(r#"<span class="product-price__current">$9.99</span>"#);
//! let found = doc2.relocate(&saved, SimilarityThreshold::default());
//! assert_eq!(found.first().map(|e| e.text()), Some("$9.99".to_string()));
//! ```

mod difflib;

use scraper::{ElementRef, Html, Selector as CssSelector};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use difflib::str_ratio;

/// Join a sequence with a separator that cannot appear inside an HTML tag name
/// or attribute value we care about — mirrors Python comparing tuples directly.
fn seq_ratio<T: AsRef<str>>(a: &[T], b: &[T]) -> f64 {
    let joined = |s: &[T]| {
        s.iter()
            .map(|x| x.as_ref().to_string())
            .collect::<Vec<_>>()
            .join("\u{1}")
    };
    str_ratio(&joined(a), &joined(b))
}

/// Minimum similarity (0–100) for [`AdaptiveDocument::relocate`] to accept a
/// candidate. Scrapling's default is 40.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimilarityThreshold(pub f64);

impl Default for SimilarityThreshold {
    fn default() -> Self {
        Self(40.0)
    }
}

/// A serialized element fingerprint — everything the similarity scorer needs.
/// Store it anywhere (file, DB, JSON column); it is plain data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ElementFingerprint {
    pub tag: String,
    /// Attribute name → value, whitespace-stripped, empties dropped (BTreeMap
    /// so serialization is deterministic).
    pub attributes: BTreeMap<String, String>,
    /// The element's own leading text (`element.text` in lxml terms), stripped.
    pub text: Option<String>,
    /// Root-to-element tag path.
    pub path: Vec<String>,
    pub parent_name: Option<String>,
    pub parent_attribs: Option<BTreeMap<String, String>>,
    pub parent_text: Option<String>,
    pub siblings: Option<Vec<String>>,
    pub children: Option<Vec<String>>,
}

impl ElementFingerprint {
    /// Parse a fingerprint previously written with `serde_json`.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serialize the fingerprint to a JSON string.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("fingerprint serialization is infallible")
    }
}

/// Errors returned by selector parsing and relocation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid CSS selector: {0}")]
    InvalidSelector(String),
}

/// A parsed HTML document with adaptive-relocation support.
pub struct AdaptiveDocument {
    html: Html,
}

/// A located element: the scraper `ElementRef` plus its owner document's tree.
pub struct AdaptiveElement<'a> {
    element: ElementRef<'a>,
}

impl AdaptiveDocument {
    pub fn parse(html: &str) -> Self {
        Self {
            html: Html::parse_document(html),
        }
    }

    /// All elements matching a CSS selector.
    pub fn css(&self, selector: &str) -> Result<Vec<AdaptiveElement<'_>>, Error> {
        let sel =
            CssSelector::parse(selector).map_err(|e| Error::InvalidSelector(e.to_string()))?;
        Ok(self
            .html
            .select(&sel)
            .map(|element| AdaptiveElement { element })
            .collect())
    }

    /// First element matching a CSS selector.
    pub fn css_first(&self, selector: &str) -> Result<Option<AdaptiveElement<'_>>, Error> {
        Ok(self.css(selector)?.into_iter().next())
    }

    /// Find the elements most similar to `fingerprint`, accepting anything at
    /// or above `threshold`. Returns the best-scoring group (there may be
    /// ties), highest first; empty when nothing clears the bar.
    pub fn relocate(
        &self,
        fingerprint: &ElementFingerprint,
        threshold: SimilarityThreshold,
    ) -> Vec<AdaptiveElement<'_>> {
        let mut best_score = f64::NEG_INFINITY;
        let mut best: Vec<ElementRef<'_>> = Vec::new();
        for node in self.html.tree.nodes() {
            let Some(element) = ElementRef::wrap(node) else {
                continue;
            };
            let score = similarity_score(fingerprint, &candidate_fingerprint(element));
            if score > best_score {
                best_score = score;
                best.clear();
                best.push(element);
            } else if score == best_score {
                best.push(element);
            }
        }
        if best_score >= threshold.0 {
            best.into_iter()
                .map(|element| AdaptiveElement { element })
                .collect()
        } else {
            Vec::new()
        }
    }
}

impl<'a> AdaptiveElement<'a> {
    /// The element's tag name.
    pub fn tag(&self) -> String {
        self.element.value().name().to_string()
    }

    /// All descendant text content, whitespace-collapsed.
    pub fn text(&self) -> String {
        self.element.text().collect::<String>().trim().to_string()
    }

    /// An attribute value by name, if present.
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.element.value().attr(name)
    }

    /// Inner HTML of the element.
    pub fn html(&self) -> String {
        self.element.inner_html()
    }

    /// Capture the fingerprint for later relocation.
    pub fn fingerprint(&self) -> ElementFingerprint {
        candidate_fingerprint(self.element)
    }

    /// The wrapped `scraper` element, for anything this API doesn't cover.
    pub fn as_element_ref(&self) -> ElementRef<'a> {
        self.element
    }
}

/// Build a fingerprint from a live element — this mirrors Scrapling's
/// `_StorageTools.element_to_dict` exactly (same fields, same cleaning rules),
/// with one documented divergence: scraper's tree has no parent pointers
/// upward from the root, so the root element's parent fields are `None` where
/// lxml would report the synthetic root; this only affects scoring when the
/// SAVED element was itself the document root.
fn candidate_fingerprint(element: ElementRef<'_>) -> ElementFingerprint {
    let value = element.value();
    let mut attributes = BTreeMap::new();
    for (name, attr_value) in value.attrs() {
        let stripped = attr_value.trim();
        if !stripped.is_empty() {
            attributes.insert(name.to_string(), stripped.to_string());
        }
    }
    // Leading text node(s): scraper exposes text as an iterator over all
    // descendant text; lxml's `element.text` is only the FIRST text chunk.
    // We take the first non-empty chunk to match lxml semantics.
    let first_text = element
        .text()
        .map(|t| t.trim())
        .find(|t| !t.is_empty())
        .map(|t| t.to_string());

    // Path: root-to-element tags.
    let mut path = Vec::new();
    for ancestor in element.ancestors() {
        if let Some(ancestor_el) = ElementRef::wrap(ancestor) {
            path.push(ancestor_el.value().name().to_string());
        }
    }
    path.reverse();

    let parent = element.parent().and_then(ElementRef::wrap);
    let (parent_name, parent_attribs, parent_text, siblings) = match parent {
        Some(parent_el) => {
            let mut parent_attribs = BTreeMap::new();
            for (name, attr_value) in parent_el.value().attrs() {
                parent_attribs.insert(name.to_string(), attr_value.to_string());
            }
            let parent_text = parent_el
                .text()
                .map(|t| t.trim())
                .find(|t| !t.is_empty())
                .map(|t| t.to_string());
            let mut siblings = Vec::new();
            for sibling in parent_el.children() {
                if let Some(sibling_el) = ElementRef::wrap(sibling) {
                    if sibling_el != element {
                        siblings.push(sibling_el.value().name().to_string());
                    }
                }
            }
            (
                Some(parent_el.value().name().to_string()),
                Some(parent_attribs),
                parent_text,
                if siblings.is_empty() {
                    None
                } else {
                    Some(siblings)
                },
            )
        }
        None => (None, None, None, None),
    };

    let mut children = Vec::new();
    for child in element.children() {
        if let Some(child_el) = ElementRef::wrap(child) {
            children.push(child_el.value().name().to_string());
        }
    }

    ElementFingerprint {
        tag: value.name().to_string(),
        attributes,
        text: first_text,
        path,
        parent_name,
        parent_attribs,
        parent_text,
        siblings,
        children: if children.is_empty() {
            None
        } else {
            Some(children)
        },
    }
}

/// The similarity score, 0–100 — a faithful port of Scrapling's
/// `__calculate_similarity_score`. Each comparison adds a score in [0,1] and
/// one "check"; the result is (score/checks) * 100, rounded to 2 decimals.
fn similarity_score(original: &ElementFingerprint, candidate: &ElementFingerprint) -> f64 {
    let mut score = 0.0f64;
    let mut checks = 0usize;

    score += if original.tag == candidate.tag {
        1.0
    } else {
        0.0
    };
    checks += 1;

    if let Some(original_text) = &original.text {
        score += str_ratio(original_text, candidate.text.as_deref().unwrap_or(""));
        checks += 1;
    }

    score += dict_diff(&original.attributes, &candidate.attributes);
    checks += 1;

    // Separate similarity for high-signal attributes — survives full
    // structural changes where paths and siblings mean nothing.
    for attrib in ["class", "id", "href", "src"] {
        if let Some(original_value) = original.attributes.get(attrib) {
            score += str_ratio(
                original_value,
                candidate
                    .attributes
                    .get(attrib)
                    .map(String::as_str)
                    .unwrap_or(""),
            );
            checks += 1;
        }
    }

    score += seq_ratio(&original.path, &candidate.path);
    checks += 1;

    if let Some(parent_name) = &original.parent_name {
        if candidate.parent_name.is_some() {
            score += str_ratio(parent_name, candidate.parent_name.as_deref().unwrap_or(""));
            checks += 1;
            let empty_map: BTreeMap<String, String> = BTreeMap::new();
            score += dict_diff(
                original.parent_attribs.as_ref().unwrap_or(&empty_map),
                candidate.parent_attribs.as_ref().unwrap_or(&empty_map),
            );
            checks += 1;
            if let Some(parent_text) = &original.parent_text {
                score += str_ratio(parent_text, candidate.parent_text.as_deref().unwrap_or(""));
                checks += 1;
            }
        }
    }

    if let Some(siblings) = &original.siblings {
        let empty: Vec<String> = Vec::new();
        let candidate_siblings = candidate.siblings.as_ref().unwrap_or(&empty);
        score += seq_ratio(siblings, candidate_siblings);
        checks += 1;
    }

    // How % sure? Scrapling rounds to 2 decimals.
    ((score / checks as f64) * 100.0 * 100.0).round() / 100.0
}

/// Scrapling's `__calculate_dict_diff`: keys-sequence ratio * 0.5 +
/// values-sequence ratio * 0.5.
fn dict_diff(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> f64 {
    let keys = seq_ratio(&a.keys().collect::<Vec<_>>(), &b.keys().collect::<Vec<_>>());
    let values = seq_ratio(
        &a.values().collect::<Vec<_>>(),
        &b.values().collect::<Vec<_>>(),
    );
    keys * 0.5 + values * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEFORE: &str = r#"
        <html><body>
            <div class="product">
                <h2 class="title">Widget</h2>
                <span class="price">$9.99</span>
            </div>
        </body></html>
    "#;
    const REDESIGNED: &str = r#"
        <html><body>
            <section class="product-card">
                <h3 class="product-title">Widget</h3>
                <span class="product-price__current">$9.99</span>
            </section>
        </body></html>
    "#;

    #[test]
    fn relocates_after_redesign() {
        let doc = AdaptiveDocument::parse(BEFORE);
        let saved = doc
            .css_first("span.price")
            .unwrap()
            .expect("price exists before")
            .fingerprint();

        let doc2 = AdaptiveDocument::parse(REDESIGNED);
        let found = doc2.relocate(&saved, SimilarityThreshold::default());
        assert!(
            !found.is_empty(),
            "redesign should still relocate the price"
        );
        assert_eq!(found[0].text(), "$9.99");
        assert_eq!(found[0].attribute("class"), Some("product-price__current"));
    }

    #[test]
    fn selector_that_still_matches_outperforms_relocation() {
        // Sanity: when the selector still works, plain css finds it directly.
        let doc = AdaptiveDocument::parse(BEFORE);
        assert_eq!(
            doc.css_first("span.price").unwrap().unwrap().text(),
            "$9.99"
        );
    }

    #[test]
    fn threshold_rejects_weak_matches() {
        let doc = AdaptiveDocument::parse(BEFORE);
        let saved = doc.css_first("span.price").unwrap().unwrap().fingerprint();
        let unrelated =
            AdaptiveDocument::parse("<html><body><footer>legal text</footer></body></html>");
        assert!(unrelated
            .relocate(&saved, SimilarityThreshold(99.0))
            .is_empty());
    }

    #[test]
    fn fingerprint_round_trips_through_json() {
        let doc = AdaptiveDocument::parse(BEFORE);
        let fp = doc.css_first("span.price").unwrap().unwrap().fingerprint();
        let json = fp.to_json();
        let back = ElementFingerprint::from_json(&json).unwrap();
        assert_eq!(fp, back);
    }
}
