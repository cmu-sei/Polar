/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! The canonical encoding of a payload: what is signed.
//!
//! UTF-8 JSON with object keys sorted by UTF-16 code unit (RFC 8785's order), no whitespace, strings
//! escaped minimally (`"` `\` and the control characters, with `\b \f \n \r \t` where they exist and
//! lower-case `\u00xx` for the rest; everything else, DEL and U+2028 included, as itself). Numbers are
//! integers only: a float has more than one spelling, and configuration has no need of one, so one is
//! refused rather than normalised.

use serde_json::Value;

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CanonicalError {
    #[error("{0} is not an integer, and only integers can be signed")]
    NotAnInteger(String),
    #[error("the payload is nested more than {MAX_DEPTH} deep")]
    TooDeep,
}

pub fn encode(value: &Value) -> Result<Vec<u8>, CanonicalError> {
    let mut out = String::new();
    write(value, &mut out, 0)?;
    Ok(out.into_bytes())
}

fn write(value: &Value, out: &mut String, depth: usize) -> Result<(), CanonicalError> {
    if depth > MAX_DEPTH {
        return Err(CanonicalError::TooDeep);
    }
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                out.push_str(&u.to_string());
            } else {
                return Err(CanonicalError::NotAnInteger(n.to_string()));
            }
        }
        Value::String(s) => string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out, depth + 1)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                string(key, out);
                out.push(':');
                write(&map[key], out, depth + 1)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(value: Value) -> String {
        String::from_utf8(encode(&value).unwrap()).unwrap()
    }

    #[test]
    fn keys_are_sorted_and_there_is_no_whitespace() {
        assert_eq!(
            text(json!({"b": 1, "a": {"d": [1, 2], "c": null}})),
            r#"{"a":{"c":null,"d":[1,2]},"b":1}"#
        );
    }

    #[test]
    fn the_order_the_keys_were_built_in_does_not_matter() {
        let mut one = serde_json::Map::new();
        one.insert("x".into(), json!(1));
        one.insert("y".into(), json!(2));
        let mut two = serde_json::Map::new();
        two.insert("y".into(), json!(2));
        two.insert("x".into(), json!(1));
        assert_eq!(
            encode(&Value::Object(one)).unwrap(),
            encode(&Value::Object(two)).unwrap()
        );
    }

    #[test]
    fn arrays_keep_their_order() {
        assert_eq!(text(json!([3, 1, 2])), "[3,1,2]");
    }

    #[test]
    fn strings_are_escaped_minimally() {
        assert_eq!(
            text(json!("a\"b\\c\n\t\r\u{8}\u{c}\u{1}\u{1f}")),
            r#""a\"b\\c\n\t\r\b\f\u0001\u001f""#
        );
        // Not escaped: DEL, the line separator, non-ASCII and supplementary-plane characters.
        assert_eq!(
            text(json!("\u{7f}\u{2028}é日本\u{1F600}")),
            "\"\u{7f}\u{2028}é日本\u{1F600}\""
        );
    }

    #[test]
    fn keys_sort_by_utf16_code_unit_not_by_utf8_byte() {
        // U+FF5E is one UTF-16 unit (0xFF5E); U+1F600 is the surrogate pair 0xD83D 0xDE00. In UTF-16
        // order the emoji sorts first; in UTF-8 byte order it sorts last.
        let value = json!({"\u{ff5e}": 1, "\u{1F600}": 2});
        assert_eq!(text(value), "{\"\u{1F600}\":2,\"\u{ff5e}\":1}");
    }

    #[test]
    fn integers_are_written_plainly_and_floats_are_refused() {
        assert_eq!(
            text(json!([0, -7, 18446744073709551615u64, i64::MIN])),
            "[0,-7,18446744073709551615,-9223372036854775808]"
        );
        assert!(matches!(
            encode(&json!(1.5)),
            Err(CanonicalError::NotAnInteger(_))
        ));
        // Even a float that is a whole number has a second spelling, so it is refused too.
        assert!(matches!(
            encode(&json!(2.0)),
            Err(CanonicalError::NotAnInteger(_))
        ));
    }

    #[test]
    fn a_payload_nested_too_deeply_is_refused() {
        let mut value = json!(1);
        for _ in 0..(MAX_DEPTH + 2) {
            value = json!([value]);
        }
        assert_eq!(encode(&value), Err(CanonicalError::TooDeep));
    }

    #[test]
    fn the_encoding_is_stable_across_a_parse_and_a_re_encode() {
        let value = json!({"agent":"a","version":3,"changes":[{"name":"X","value":"1"}]});
        let once = encode(&value).unwrap();
        let again = encode(&serde_json::from_slice::<Value>(&once).unwrap()).unwrap();
        assert_eq!(once, again);
    }
}
