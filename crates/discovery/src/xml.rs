//! Minimal namespace-agnostic XML reading for the discovery bodies.

use quick_xml::Reader;
use quick_xml::events::Event;

/// Malformed XML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Malformed;

/// Upper bound on elements read from one datagram, so a hostile datagram
/// cannot make a parse take long.
const MAX_ELEMENTS: usize = 4096;

/// Every element's text, as `(path, text)` in document order, where `path`
/// joins the local names (prefixes dropped) from the root with `/`. Text is
/// trimmed; references (`&amp;`, `&#38;`) are resolved. Elements without
/// text appear with an empty string. The document must be well formed.
pub(crate) fn leaf_texts(xml: &str) -> Result<Vec<(String, String)>, Malformed> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<String> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    let mut out = Vec::new();
    let mut seen_root = false;
    loop {
        match reader.read_event().map_err(|_| Malformed)? {
            Event::Start(e) => {
                if stack.is_empty() && seen_root {
                    return Err(Malformed);
                }
                seen_root = true;
                stack.push(String::from_utf8_lossy(e.local_name().as_ref()).into_owned());
                texts.push(String::new());
                if stack.len() > 64 || out.len() > MAX_ELEMENTS {
                    return Err(Malformed);
                }
            }
            Event::Empty(e) => {
                if stack.is_empty() {
                    if seen_root {
                        return Err(Malformed);
                    }
                    seen_root = true;
                }
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                let path = if stack.is_empty() {
                    name
                } else {
                    format!("{}/{name}", stack.join("/"))
                };
                out.push((path, String::new()));
                if out.len() > MAX_ELEMENTS {
                    return Err(Malformed);
                }
            }
            Event::Text(t) => {
                if let Some(text) = texts.last_mut() {
                    text.push_str(&t.xml10_content().map_err(|_| Malformed)?);
                }
            }
            Event::CData(t) => {
                if let Some(text) = texts.last_mut() {
                    text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::GeneralRef(r) => {
                let resolved = if let Some(c) = r.resolve_char_ref().map_err(|_| Malformed)? {
                    c.to_string()
                } else {
                    let name = r.xml10_content().map_err(|_| Malformed)?;
                    quick_xml::escape::resolve_predefined_entity(&name)
                        .ok_or(Malformed)?
                        .to_owned()
                };
                if let Some(text) = texts.last_mut() {
                    text.push_str(&resolved);
                }
            }
            Event::End(_) => {
                let path = stack.join("/");
                stack.pop().ok_or(Malformed)?;
                let text = texts.pop().ok_or(Malformed)?;
                out.push((path, text.trim().to_owned()));
            }
            Event::Eof => {
                return if stack.is_empty() && seen_root {
                    Ok(out)
                } else {
                    Err(Malformed)
                };
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_texts() {
        let x = "<?xml version=\"1.0\"?><a:R xmlns:a=\"u\"><a:B> x &amp; y </a:B><C/><D><E>1</E></D></a:R>";
        let t = leaf_texts(x).unwrap();
        assert!(t.contains(&("R/B".into(), "x & y".into())));
        assert!(t.contains(&("R/C".into(), String::new())));
        assert!(t.contains(&("R/D/E".into(), "1".into())));
    }

    #[test]
    fn malformed_documents_fail() {
        assert!(leaf_texts("<a><b></a>").is_err());
        assert!(leaf_texts("<a>").is_err());
        assert!(leaf_texts("").is_err());
        assert!(leaf_texts("<a></a><b></b>").is_err());
        assert!(leaf_texts("<a/><b/>").is_err());
        assert_eq!(
            leaf_texts("<a/>").unwrap(),
            vec![("a".into(), String::new())]
        );
    }
}
