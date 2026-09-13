const CODE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cs", "css", "go", "h", "hpp", "htm", "html", "java", "js", "json", "jsx",
    "kt", "lock", "lua", "md", "mjs", "php", "ps1", "py", "rb", "rs", "sass", "scss", "sh", "sql",
    "svelte", "svg", "swift", "toml", "ts", "tsx", "vue", "xml", "yaml", "yml",
];

/// Wraps the words in `text` that can only be code in backticks, leaving everything else as it is.
///
/// Models write their reasoning as plain prose and rarely quote what they mention, so `<pre>`,
/// `@keyframes` or `planet.html` would otherwise read as ordinary words. Only shapes that prose
/// does not produce are marked, and nothing inside a code span, a fenced block or an indented
/// block is touched, so a wrong guess costs one monospace word rather than a mangled sentence.
pub(crate) fn mark_code(text: &str) -> String {
    let mut marked = String::with_capacity(text.len() + text.len() / 8);
    let mut in_fence = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            marked.push_str(line);
        } else if in_fence || line.starts_with("    ") || line.starts_with('\t') {
            marked.push_str(line);
        } else {
            mark_line(line, &mut marked);
        }
    }
    marked
}

fn mark_line(line: &str, marked: &mut String) {
    let mut in_code_span = false;
    let mut rest = line;
    while let Some(first) = rest.chars().next() {
        let length = if first.is_whitespace() {
            rest.find(|character: char| !character.is_whitespace())
                .unwrap_or(rest.len())
        } else {
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        };
        let (segment, remainder) = rest.split_at(length);
        rest = remainder;

        if first.is_whitespace() {
            marked.push_str(segment);
            continue;
        }

        // A span the model quoted itself can hold spaces, so the words after its opening backtick
        // belong to it until one closes it.
        let backticks = segment.matches('`').count();
        if in_code_span || backticks > 0 {
            marked.push_str(segment);
            if backticks % 2 == 1 {
                in_code_span = !in_code_span;
            }
            continue;
        }

        let (leading, core, trailing) = split_punctuation(segment);
        if is_code(core) {
            marked.push_str(leading);
            marked.push('`');
            marked.push_str(core);
            marked.push('`');
            marked.push_str(trailing);
        } else {
            marked.push_str(segment);
        }
    }
}

/// Separates the quotes and punctuation around a word from the word, so `planet.html.` marks as
/// `` `planet.html`. `` and a closing parenthesis only leaves when nothing inside opened it.
fn split_punctuation(word: &str) -> (&str, &str, &str) {
    let core_start = word
        .char_indices()
        .find(|(_, character)| !matches!(character, '(' | '[' | '"' | '\'' | '“' | '‘'))
        .map_or(word.len(), |(index, _)| index);
    let (leading, rest) = word.split_at(core_start);

    let mut core = rest;
    while let Some(last) = core.chars().next_back() {
        let unbalanced_parenthesis =
            last == ')' && core.matches('(').count() < core.matches(')').count();
        let is_punctuation = matches!(
            last,
            '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' | '”' | '’' | ']'
        );
        if !is_punctuation && !unbalanced_parenthesis {
            break;
        }
        core = core
            .get(..core.len() - last.len_utf8())
            .unwrap_or_default();
    }
    let trailing = rest.get(core.len()..).unwrap_or_default();
    (leading, core, trailing)
}

fn is_code(word: &str) -> bool {
    if word.chars().count() < 2 || word.contains("://") {
        return false;
    }
    is_html_tag(word)
        || is_at_rule(word)
        || is_flag(word)
        || is_call(word)
        || (word.contains("::") && is_identifier_path(word))
        || is_snake_case(word)
        || is_file_name(word)
}

fn is_html_tag(word: &str) -> bool {
    let Some(inner) = word
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
    else {
        return false;
    };
    let inner = inner.strip_prefix('/').unwrap_or(inner);
    let inner = inner.strip_suffix('/').unwrap_or(inner);
    inner.starts_with(|character: char| character.is_ascii_alphabetic())
        && inner
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn is_at_rule(word: &str) -> bool {
    word.strip_prefix('@').is_some_and(|name| {
        name.starts_with(|character: char| character.is_ascii_lowercase())
            && name
                .chars()
                .all(|character| character.is_ascii_lowercase() || character == '-')
    })
}

fn is_flag(word: &str) -> bool {
    word.strip_prefix("--").is_some_and(|name| {
        name.len() >= 2
            && name.starts_with(|character: char| character.is_ascii_alphabetic())
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
    })
}

/// `draw()` and `console.log(x)` are calls, but prose writes `arquivo(s)` and `file(s)` too, so a
/// plain word with something in its parentheses is only a call when its name reads as code.
fn is_call(word: &str) -> bool {
    let Some((callee, arguments)) = word.split_once('(') else {
        return false;
    };
    let named_like_code = callee.contains(['.', '_', ':'])
        || callee
            .chars()
            .skip(1)
            .any(|character| character.is_ascii_uppercase());
    arguments.ends_with(')') && is_identifier_path(callee) && (arguments == ")" || named_like_code)
}

fn is_identifier_path(text: &str) -> bool {
    text.starts_with(|character: char| character.is_ascii_alphabetic() || character == '_')
        && text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | ':'))
}

/// Leading and trailing underscores are markdown emphasis, not an identifier.
fn is_snake_case(word: &str) -> bool {
    word.contains('_')
        && !word.starts_with('_')
        && !word.ends_with('_')
        && word.chars().any(|character| character.is_ascii_alphabetic())
        && word
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn is_file_name(word: &str) -> bool {
    let Some((stem, extension)) = word.rsplit_once('.') else {
        return false;
    };
    let name = stem.rsplit(['/', '\\']).next().unwrap_or(stem);
    !name.is_empty()
        && stem.chars().any(|character| character.is_ascii_alphabetic())
        && stem.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '/' | '\\')
        })
        && CODE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::mark_code;

    #[test]
    fn marks_what_can_only_be_code() {
        assert_eq!(
            mark_code("Rotate a <pre> or <div> with CSS @keyframes, then write planet.html."),
            "Rotate a `<pre>` or `<div>` with CSS `@keyframes`, then write `planet.html`."
        );
        assert_eq!(
            mark_code("Call requestAnimationFrame() and console.log(frame) in src/main.rs"),
            "Call `requestAnimationFrame()` and `console.log(frame)` in `src/main.rs`"
        );
        assert_eq!(
            mark_code("Use write_file, std::fs and --foreground (or </body>)."),
            "Use `write_file`, `std::fs` and `--foreground` (or `</body>`)."
        );
    }

    #[test]
    fn leaves_prose_alone() {
        for text in [
            "Crie o(s) arquivo(s), e.g. um só.",
            "O código parece correto. Não há necessidade de mais alterações.",
            "It runs 3.5 times faster, see https://models.dev/api.json for the list.",
            "Mail user@example.com about the _emphasis_ and **bold** words.",
        ] {
            assert_eq!(mark_code(text), text);
        }
    }

    #[test]
    fn leaves_code_the_model_quoted_alone() {
        assert_eq!(
            mark_code("Set `animation: spin 8s linear` on <div> and `write_file` too.\n"),
            "Set `animation: spin 8s linear` on `<div>` and `write_file` too.\n"
        );
        assert_eq!(
            mark_code("Before <span>\n```html\n<div> planet.html\n```\n    indented <b>\nAfter <p>"),
            "Before `<span>`\n```html\n<div> planet.html\n```\n    indented <b>\nAfter `<p>`"
        );
    }
}
