//! The Studio application (documents) as capabilities.

use mouli_interface::{Application, Call, CapabilitySpec, Error, Output, Result, Risk};
use mouli_studio::{Block, Document};
use std::cell::RefCell;
use std::rc::Rc;

/// The document, shared between the adapter and the screen.
pub type SharedDoc = Rc<RefCell<Document>>;

/// Longest text of one block, in bytes.
const MAX_BLOCK: usize = 4000;
/// Most blocks in the document.
const MAX_BLOCKS: usize = 2000;

/// The Studio application.
pub struct StudioApp {
    doc: SharedDoc,
}

impl StudioApp {
    /// An application editing `doc`.
    pub fn new(doc: SharedDoc) -> Self {
        Self { doc }
    }
}

impl Application for StudioApp {
    fn name(&self) -> &str {
        "studio"
    }

    fn capabilities(&self) -> Vec<CapabilitySpec> {
        vec![
            CapabilitySpec::new("studio.show", "show the document", Risk::Read),
            CapabilitySpec::new("studio.heading", "add a heading (text=...)", Risk::Write)
                .requires(&["text"]),
            CapabilitySpec::new(
                "studio.paragraph",
                "add a paragraph (text=...)",
                Risk::Write,
            )
            .requires(&["text"]),
            CapabilitySpec::new(
                "studio.clear",
                "erase the whole document; needs approval",
                Risk::Consequential,
            ),
        ]
    }

    fn invoke(&mut self, call: &Call) -> Result<Output> {
        let mut doc = self.doc.borrow_mut();
        let text = match call.capability.as_str() {
            "studio.show" => {
                let t = doc.render_plain();
                if t.trim().is_empty() {
                    "(the document is empty)".to_string()
                } else {
                    t
                }
            }
            cap @ ("studio.heading" | "studio.paragraph") => {
                let text = call.args.get("text").map_or("", String::as_str).trim();
                if text.is_empty() {
                    return Err(Error::Invalid("text is empty".into()));
                }
                if text.len() > MAX_BLOCK {
                    return Err(Error::Limit(format!(
                        "a block may be at most {MAX_BLOCK} bytes"
                    )));
                }
                if doc.blocks.len() >= MAX_BLOCKS {
                    return Err(Error::Limit(format!(
                        "the document is full ({MAX_BLOCKS} blocks)"
                    )));
                }
                doc.blocks.push(if cap == "studio.heading" {
                    Block::Heading(text.to_string())
                } else {
                    Block::Paragraph(text.to_string())
                });
                format!("added block {}", doc.blocks.len())
            }
            "studio.clear" => {
                let n = doc.blocks.len();
                doc.blocks.clear();
                format!("removed {n} block(s)")
            }
            other => return Err(Error::NotFound(format!("capability {other}"))),
        };
        Ok(Output {
            text,
            artifacts: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn call(cap: &str, text: Option<&str>) -> Call {
        Call {
            capability: cap.into(),
            args: text
                .map(|t| ("text".to_string(), t.to_string()))
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn app() -> (StudioApp, SharedDoc) {
        let d: SharedDoc = Rc::new(RefCell::new(Document::default()));
        (StudioApp::new(d.clone()), d)
    }

    #[test]
    fn blocks_are_added_in_order_and_shown() {
        let (mut a, d) = app();
        assert_eq!(
            a.invoke(&call("studio.show", None)).unwrap().text,
            "(the document is empty)"
        );
        a.invoke(&call("studio.heading", Some("  Plan  "))).unwrap();
        a.invoke(&call("studio.paragraph", Some("First step.")))
            .unwrap();
        assert_eq!(
            d.borrow().blocks,
            vec![
                Block::Heading("Plan".into()),
                Block::Paragraph("First step.".into())
            ]
        );
        let shown = a.invoke(&call("studio.show", None)).unwrap().text;
        assert!(
            shown.contains("Plan") && shown.contains("First step."),
            "{shown}"
        );
    }

    #[test]
    fn empty_and_oversized_text_is_refused() {
        let (mut a, d) = app();
        assert!(matches!(
            a.invoke(&call("studio.paragraph", Some("   "))),
            Err(Error::Invalid(_))
        ));
        let big = "x".repeat(MAX_BLOCK + 1);
        assert!(matches!(
            a.invoke(&call("studio.paragraph", Some(&big))),
            Err(Error::Limit(_))
        ));
        assert!(d.borrow().blocks.is_empty());
    }

    #[test]
    fn the_document_has_a_size_limit() {
        let (mut a, d) = app();
        d.borrow_mut().blocks = vec![Block::Paragraph("x".into()); MAX_BLOCKS];
        assert!(matches!(
            a.invoke(&call("studio.heading", Some("one more"))),
            Err(Error::Limit(_))
        ));
    }

    #[test]
    fn clear_is_consequential_and_reports_the_count() {
        let (mut a, d) = app();
        a.invoke(&call("studio.paragraph", Some("a"))).unwrap();
        a.invoke(&call("studio.paragraph", Some("b"))).unwrap();
        assert!(a
            .capabilities()
            .iter()
            .any(|c| c.name == "studio.clear" && c.risk == Risk::Consequential));
        assert_eq!(
            a.invoke(&call("studio.clear", None)).unwrap().text,
            "removed 2 block(s)"
        );
        assert!(d.borrow().blocks.is_empty());
    }
}
