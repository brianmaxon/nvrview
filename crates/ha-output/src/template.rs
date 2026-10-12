//! Message templates. Placeholders are filled in one pass from typed alert
//! facts, so a value that happens to contain `{...}` is never expanded, and
//! every value is cleaned of control characters and capped.

const NAMES: [&str; 5] = ["camera", "class", "time", "severity", "explanation"];
const MAX_TEMPLATE: usize = 500;
const MAX_VALUE: usize = 200;

pub(crate) struct Facts<'a> {
    pub camera: &'a str,
    pub class: &'a str,
    pub time: &'a str,
    pub severity: &'a str,
    pub explanation: &'a str,
}

impl Facts<'_> {
    fn get(&self, name: &str) -> &str {
        match name {
            "camera" => self.camera,
            "class" => self.class,
            "time" => self.time,
            "severity" => self.severity,
            _ => self.explanation,
        }
    }
}

pub(crate) fn validate(t: &str) -> Result<(), &'static str> {
    if t.chars().count() > MAX_TEMPLATE {
        return Err("too long");
    }
    let mut rest = t;
    while let Some(i) = rest.find(['{', '}']) {
        if !rest[i..].starts_with('{') {
            return Err("unmatched }");
        }
        let after = &rest[i + 1..];
        let Some(end) = after.find('}') else {
            return Err("unmatched {");
        };
        if !NAMES.contains(&&after[..end]) {
            return Err("unknown placeholder");
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

/// Replaces control characters (including newlines) with spaces and caps
/// the length.
pub(crate) fn clean(v: &str) -> String {
    let s: String = v
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let s = s.trim();
    if s.chars().count() > MAX_VALUE {
        let cut: String = s.chars().take(MAX_VALUE - 1).collect();
        format!("{cut}…")
    } else {
        s.to_string()
    }
}

/// `template` must have passed [`validate`].
pub(crate) fn render(template: &str, facts: &Facts) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let end = after.find('}').unwrap_or(0);
        out.push_str(&clean(facts.get(&after[..end])));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}
