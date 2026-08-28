//! Reduction of seller HTML to its visible text.
//!
//! eBay's item `description` is a complete seller-authored HTML document —
//! store template, inline stylesheets, navigation — routinely tens of
//! kilobytes wrapping a few sentences of substance. The response models are
//! a deliberate projection to keep context costs down, and passing raw HTML
//! through that projection defeats it, so only the rendered text survives.
//!
//! This is not a general HTML renderer: it targets what listing descriptions
//! actually contain, and on malformed markup it errs toward dropping content
//! rather than leaking markup into the text.

/// Elements whose entire content is invisible, not just their tags.
const INVISIBLE_ELEMENTS: [&str; 6] = ["script", "style", "head", "title", "svg", "noscript"];

/// Elements that end a line when rendered, approximated with a newline so
/// paragraph structure survives.
const BLOCK_ELEMENTS: [&str; 20] = [
    "p",
    "br",
    "div",
    "li",
    "ul",
    "ol",
    "tr",
    "table",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "section",
    "article",
    "header",
    "footer",
    "hr",
    "blockquote",
];

/// Strip `html` down to roughly the text a browser would render: tags
/// removed (block-level ones becoming line breaks), invisible elements
/// dropped with their content, entities decoded, whitespace collapsed.
pub fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len() / 4);
    let mut rest = html;

    while let Some(lt) = rest.find('<') {
        text.push_str(&rest[..lt]);
        rest = &rest[lt..];

        if let Some(after) = rest.strip_prefix("<!--") {
            rest = match after.find("-->") {
                Some(end) => &after[end + 3..],
                None => "",
            };
            continue;
        }

        let Some(gt) = rest.find('>') else {
            // A dangling `<` opens a tag that never closes; nothing after it
            // is text.
            rest = "";
            break;
        };
        let tag = &rest[1..gt];
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        rest = &rest[gt + 1..];

        if !tag.starts_with('/') && INVISIBLE_ELEMENTS.contains(&name.as_str()) {
            let close = format!("</{name}");
            rest = match find_ignore_case(rest, &close) {
                Some(start) => {
                    let after = &rest[start..];
                    match after.find('>') {
                        Some(end) => &after[end + 1..],
                        None => "",
                    }
                }
                None => "",
            };
            continue;
        }

        text.push(if BLOCK_ELEMENTS.contains(&name.as_str()) {
            '\n'
        } else {
            ' '
        });
    }
    text.push_str(rest);

    collapse_whitespace(&decode_entities(&text))
}

/// Byte offset of the first case-insensitive occurrence of `needle`.
/// Tag names are ASCII, so per-byte folding suffices; a match starting on
/// `<` is always a char boundary.
fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Decode the entities that actually occur in listing HTML. An
/// unrecognizable `&`-sequence passes through literally rather than
/// vanishing.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|end| *end <= 32).and_then(|end| {
            let ch = match &rest[1..end] {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                entity => entity.strip_prefix('#').and_then(decode_numeric),
            };
            ch.map(|ch| (ch, end + 1))
        });
        match decoded {
            Some((ch, len)) => {
                out.push(ch);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_numeric(digits: &str) -> Option<char> {
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse().ok()?,
    };
    char::from_u32(code)
}

/// Collapse each whitespace run to one character — a newline if the run
/// contained one, a space otherwise — and drop leading and trailing runs.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut separator: Option<char> = None;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if separator != Some('\n') {
                separator = Some(if ch == '\n' { '\n' } else { ' ' });
            }
        } else {
            if let Some(sep) = separator.take()
                && !out.is_empty()
            {
                out.push(sep);
            }
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "panicking is how a test reports failure"
    )]

    use super::*;

    #[test]
    fn tags_vanish_and_text_survives() {
        assert_eq!(html_to_text("<p>Hello <b>world</b></p>"), "Hello world");
    }

    #[test]
    fn invisible_elements_take_their_content_with_them() {
        let html = "<STYLE>.a{color:#000}</STYLE><p>Real</p><script>var x = '<p>';</script>";
        assert_eq!(html_to_text(html), "Real");
    }

    #[test]
    fn block_tags_become_line_breaks() {
        assert_eq!(
            html_to_text("<p>one</p><p>two</p>three<br>four"),
            "one\ntwo\nthree\nfour"
        );
    }

    #[test]
    fn entities_decode_and_unknown_ones_pass_through() {
        assert_eq!(
            html_to_text("A &amp; B &lt;kbd&gt; &#9733; &#x2605; &wat; C"),
            "A & B <kbd> ★ ★ &wat; C"
        );
    }

    #[test]
    fn comments_and_dangling_markup_do_not_leak() {
        assert_eq!(html_to_text("a<!-- <p>hidden</p> -->b"), "ab");
        assert_eq!(html_to_text("text<div class="), "text");
        assert_eq!(html_to_text("<style>orphaned"), "");
    }

    #[test]
    fn a_store_template_reduces_to_its_sentences() {
        // Shaped like a real listing: a styled store template around a short
        // description.
        let html = r##"<font><meta content="text/html"><style type="text/css">
            #allbg{background:#EFEFEF;width:100%}</style>
            <div id="header"><a href="https://stores.example/x"><img src="logo.jpg"></a></div>
            <div class="descbox"><p><strong>★What is included:&nbsp;Keyboard x 1</strong></p>
            <p><strong>Warranty: 6 months.</strong></p></div></font>"##;
        let text = html_to_text(html);
        assert_eq!(text, "★What is included: Keyboard x 1\nWarranty: 6 months.");
    }
}
