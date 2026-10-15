//! Reading the file an author edits: JSON that may carry `//` comment lines and trailing commas, since a
//! template is full of the first and an edit by hand leaves the second. Everything else is strict JSON.

use serde_json::Value;

/// `text` with whole-line `//` comments removed and the commas that come right before a closing `}` or `]`
/// removed. A `//` inside a string is not a comment, and a comma inside one is not touched.
pub fn strip(text: &str) -> String {
    // Whole-line comments first. A line that starts, after spaces, with `//` is a comment whether or not it is
    // inside a string, which only a multi-line string could make it, and JSON has none.
    let without_comments: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    // Then trailing commas, with a small scanner that knows what is inside a string.
    let chars: Vec<char> = without_comments.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if !matches!(next, Some('}') | Some(']')) {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// Parses what an author edited.
pub fn parse(text: &str) -> Result<Value, String> {
    serde_json::from_str(&strip(text)).map_err(|e| format!("the file is not JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comment_lines_are_dropped_and_the_rest_is_read() {
        let text = "// a template\n{\n  // the agent\n  \"agent\": \"a\",\n    // another\n  \"n\": 1\n}\n";
        assert_eq!(parse(text).unwrap(), json!({"agent": "a", "n": 1}));
    }

    #[test]
    fn a_trailing_comma_before_a_closing_bracket_is_forgiven() {
        assert_eq!(
            parse("{\"a\": [1, 2,], \"b\": {\"c\": 3,},}").unwrap(),
            json!({"a": [1, 2], "b": {"c": 3}})
        );
        assert_eq!(
            parse("{\n \"a\": 1,\n // \"b\": 2\n}").unwrap(),
            json!({"a": 1})
        );
    }

    #[test]
    fn what_is_inside_a_string_is_left_alone() {
        let text = r#"{"url": "https://example.org/a//b", "list": "x,]", "q": "a \", ] b"}"#;
        assert_eq!(
            parse(text).unwrap(),
            json!({"url": "https://example.org/a//b", "list": "x,]", "q": "a \", ] b"})
        );
    }

    #[test]
    fn a_comma_between_values_stays() {
        assert_eq!(parse("[1,\n2]").unwrap(), json!([1, 2]));
    }

    #[test]
    fn text_that_is_not_json_is_refused() {
        for bad in ["", "{", "{\"a\": }", "[1 2]", "// only a comment\n"] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
