//! Reading Home Assistant's own error text out of a refused reply.

use serde_json::Value;

/// Longest error text kept, in characters.
const MAX_CHARS: usize = 160;

/// The `message` member of a JSON error body, made safe to show: control
/// characters become spaces, runs of spaces collapse, and it is cut to
/// [`MAX_CHARS`]. It is displayed only, never interpreted. `None` for a body
/// that is not a JSON object with a non-empty text `message`.
pub(crate) fn error_message(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let raw = v.get("message")?.as_str()?;
    let spaced: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut text = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > MAX_CHARS {
        text = text.chars().take(MAX_CHARS - 1).collect::<String>() + "\u{2026}";
    }
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_plain_message() {
        let m = error_message(br#"{"message":"Service notify.example_phone not found."}"#);
        assert_eq!(
            m.as_deref(),
            Some("Service notify.example_phone not found.")
        );
    }

    #[test]
    fn strips_control_characters_and_bounds_the_length() {
        let body =
            serde_json::json!({"message": format!("a\u{7}b\n\u{1b}[31mc{}", "x".repeat(400))});
        let m = error_message(body.to_string().as_bytes()).unwrap();
        assert!(!m.chars().any(char::is_control));
        assert!(m.starts_with("a b [31mc"));
        assert_eq!(m.chars().count(), MAX_CHARS);
    }

    #[test]
    fn ignores_bodies_without_text() {
        assert_eq!(error_message(b"not json"), None);
        assert_eq!(error_message(br#"{"message":5}"#), None);
        assert_eq!(error_message(br#"{"message":"  \n "}"#), None);
        assert_eq!(error_message(br#"[]"#), None);
    }
}
