//! Tool calls a model wrote into its prose instead of asking for them properly.
//!
//! Tool calling is supposed to arrive in a field of its own, and from most models it does. Some do
//! not: a family of open models — the ones that show up behind free and auto-routed endpoints —
//! were trained to emit `<tool_call>{…}</tool_call>` as *text*, and they keep doing it whether or
//! not the endpoint in front of them advertises structured calls. What the app receives is an
//! assistant message with a JSON object in the middle of a paragraph and no calls at all.
//!
//! Left alone that is the worst kind of failure, because nothing reports it. The turn ends
//! normally, the model believes it wrote the file, the user sees a wall of reasoning where an
//! answer should be, and the file was never written. So the text is read, and a call written into
//! it is treated as the call it was meant to be.
//!
//! The one rule observed throughout: never guess. A block that does not parse, or that names no
//! tool, is left exactly where it is — prose that merely looks like a call is prose.

use crate::provider::ToolCall;
use serde_json::Value;
use std::ops::Range;

/// The tag the models in question close with. The opening one is often missing, which is why the
/// search is anchored on this.
const CLOSING_TAG: &str = "</tool_call>";
const OPENING_TAG: &str = "<tool_call>";

/// A call found in the text, and the span it occupied.
pub struct Inline {
    pub call: ToolCall,
    /// What to cut from the text so the call is not also shown as prose.
    pub span: Range<usize>,
}

/// Finds the calls a model wrote into `text`.
///
/// Returned in the order they appear, with spans that do not overlap, so a caller can remove them
/// back to front without recomputing anything.
pub fn find(text: &str) -> Vec<Inline> {
    let mut found = Vec::new();
    let mut search_from = 0;

    while let Some(offset) = text[search_from..].find(CLOSING_TAG) {
        let close_start = search_from + offset;
        let close_end = close_start + CLOSING_TAG.len();
        let before = &text[search_from..close_start];

        // Prefer a real opening tag. Without one — which is the common case — fall back to the
        // last JSON object in the run of text leading up to the close.
        let object = match before.rfind(OPENING_TAG) {
            Some(open) => {
                let start = search_from + open;
                object_at(text, start + OPENING_TAG.len()).map(|body| (start, body))
            }
            None => last_object(text, search_from, close_start),
        };

        if let Some((start, body)) = object
            && let Some(call) = parse_call(&body)
        {
            found.push(Inline {
                call,
                span: start..close_end,
            });
        }

        search_from = close_end;
    }

    found
}

/// The JSON object beginning at or after `from`, if the text there is one.
///
/// Braces are matched rather than the text being scanned for a closing one, because the arguments
/// of a write call routinely contain braces inside strings — a TypeScript file, most of the time.
fn object_at(text: &str, from: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let start = (from..text.len()).find(|index| {
        let byte = bytes[*index];
        !byte.is_ascii_whitespace()
    })?;
    if bytes[start] != b'{' {
        return None;
    }

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for index in start..text.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..=index].to_owned());
                }
            }
            _ => {}
        }
    }
    None
}

/// The last JSON object that starts inside `from..until` and names a tool.
///
/// Searched from the end because the block immediately before the closing tag is the call; earlier
/// objects in the same paragraph are usually the model quoting a file it read.
fn last_object(text: &str, from: usize, until: usize) -> Option<(usize, String)> {
    let mut candidates: Vec<usize> = Vec::new();
    let mut cursor = from;
    while let Some(offset) = text[cursor..until].find('{') {
        candidates.push(cursor + offset);
        cursor += offset + 1;
    }

    for start in candidates.into_iter().rev() {
        if let Some(body) = object_at(text, start)
            && body.contains("\"name\"")
        {
            return Some((start, body));
        }
    }
    None
}

/// Turns a parsed object into a call, when it really is one.
fn parse_call(body: &str) -> Option<ToolCall> {
    let value: Value = serde_json::from_str(body).ok()?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())?;

    // Arguments arrive either as an object or, from some models, as a JSON string holding one.
    // Both are normalised to the string form the rest of the app uses.
    let arguments = match value.get("arguments").or_else(|| value.get("parameters")) {
        Some(Value::String(raw)) => raw.clone(),
        Some(other) => other.to_string(),
        None => "{}".to_owned(),
    };

    Some(ToolCall {
        // The id is ours to invent: the model never issued one, and the providers that accept a
        // result keyed by id are the same ones that would have sent a structured call in the first
        // place.
        id: format!("inline-{name}-{}", arguments.len()),
        name: name.to_owned(),
        arguments,
    })
}

/// Removes the spans a set of calls occupied.
///
/// Back to front, so an earlier span's offsets are still valid when it is reached.
pub fn strip(text: &str, found: &[Inline]) -> String {
    let mut out = text.to_owned();
    for inline in found.iter().rev() {
        out.replace_range(inline.span.clone(), "");
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_properly_tagged_call_is_found() {
        let text = "I will write the file.\n\
                    <tool_call>{\"name\": \"write\", \"arguments\": {\"path\": \"a.ts\"}}</tool_call>";

        let found = find(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].call.name, "write");
        assert_eq!(found[0].call.arguments, r#"{"path":"a.ts"}"#);
    }

    #[test]
    fn a_call_with_no_opening_tag_is_still_found() {
        // This is what actually happened: the model emitted the object and the closing tag, and
        // never opened it. Requiring the pair would have found nothing.
        let text = "The content is already known. So:\n\
                    {\"name\": \"write\", \"arguments\": {\"path\": \"./src/calculator.ts\"}} </tool_call>";

        let found = find(text);
        assert_eq!(found.len(), 1, "the untagged form is the common one");
        assert_eq!(found[0].call.name, "write");
    }

    #[test]
    fn braces_inside_the_file_being_written_do_not_end_the_object() {
        // The real failure was a call whose `contents` was a TypeScript file full of braces.
        // Scanning for the first `}` would have cut the call in half and parsed nothing.
        let contents = "export class Calculator { calculate() { return { a: 1 }; } }";
        let text = format!(
            "<tool_call>{{\"name\": \"write\", \"arguments\": {{\"path\": \"c.ts\", \"contents\": {}}}}}</tool_call>",
            serde_json::to_string(contents).unwrap()
        );

        let found = find(&text);
        assert_eq!(found.len(), 1);
        let arguments: Value = serde_json::from_str(&found[0].call.arguments).unwrap();
        assert_eq!(arguments["contents"], contents);
    }

    #[test]
    fn an_escaped_quote_inside_a_string_does_not_end_it() {
        let text = r#"<tool_call>{"name": "write", "arguments": {"contents": "say \"hi\" {"}}</tool_call>"#;

        let found = find(text);
        assert_eq!(found.len(), 1, "the unbalanced brace is inside a string");
    }

    #[test]
    fn prose_that_merely_mentions_a_tool_is_left_alone() {
        // Nothing here closes a tool call, so nothing is a tool call.
        let text = "I could use write to create the file, with {\"name\": \"write\"} as the shape.";
        assert!(find(text).is_empty());
    }

    #[test]
    fn a_block_that_does_not_parse_is_not_guessed_at() {
        let text = "<tool_call>{\"name\": \"write\", oops</tool_call>";
        assert!(find(text).is_empty());
    }

    #[test]
    fn a_block_naming_no_tool_is_not_a_call() {
        let text = "<tool_call>{\"path\": \"a.ts\"}</tool_call>";
        assert!(find(text).is_empty());
    }

    #[test]
    fn several_calls_in_one_message_are_all_found_in_order() {
        let text = "<tool_call>{\"name\":\"read\",\"arguments\":{\"path\":\"a\"}}</tool_call>\
                    and then\
                    <tool_call>{\"name\":\"read\",\"arguments\":{\"path\":\"b\"}}</tool_call>";

        let found = find(text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].call.arguments, r#"{"path":"a"}"#);
        assert_eq!(found[1].call.arguments, r#"{"path":"b"}"#);
    }

    #[test]
    fn arguments_sent_as_a_json_string_are_unwrapped() {
        // Some models double-encode, exactly as the OpenAI wire format does.
        let text = r#"<tool_call>{"name":"read","arguments":"{\"path\":\"a\"}"}</tool_call>"#;

        let found = find(text);
        assert_eq!(found[0].call.arguments, r#"{"path":"a"}"#);
    }

    #[test]
    fn a_call_with_no_arguments_gets_an_empty_object_rather_than_nothing() {
        let text = r#"<tool_call>{"name":"list"}</tool_call>"#;
        assert_eq!(find(text)[0].call.arguments, "{}");
    }

    #[test]
    fn stripping_leaves_the_prose_and_removes_the_call() {
        let text = "Let me write it.\n\
                    <tool_call>{\"name\":\"write\",\"arguments\":{\"path\":\"a\"}}</tool_call>\n\
                    That should do it.";

        let found = find(text);
        let stripped = strip(text, &found);

        assert!(stripped.contains("Let me write it."));
        assert!(stripped.contains("That should do it."));
        assert!(!stripped.contains("tool_call"), "{stripped}");
        assert!(!stripped.contains("\"name\""), "{stripped}");
    }

    #[test]
    fn stripping_several_calls_does_not_corrupt_the_text_between_them() {
        // Removing front to back would shift every later span; this is the test that catches it.
        let text = "one<tool_call>{\"name\":\"a\"}</tool_call>two<tool_call>{\"name\":\"b\"}</tool_call>three";

        let found = find(text);
        assert_eq!(strip(text, &found), "onetwothree");
    }

    #[test]
    fn the_earlier_object_in_a_paragraph_is_not_mistaken_for_the_call() {
        // A model often quotes a file it read, then writes the call. Without an opening tag the
        // search has to take the *last* object, not the first.
        let text = "I read {\"name\": \"simple-website\", \"version\": \"1.0.0\"} from package.json. \
                    Now: {\"name\": \"write\", \"arguments\": {\"path\": \"a.ts\"}}</tool_call>";

        let found = find(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].call.name, "write");
    }
}
