//! Safe markdown → HTML / code parts for chat (no raw HTML, http(s) links only).

use pulldown_cmark::{CodeBlockKind, CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Clone, Debug, PartialEq)]
pub enum MdPart {
    Html(String),
    Code { lang: String, code: String },
}

pub fn markdown_to_parts(input: &str) -> Vec<MdPart> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(input, opts);

    let mut parts = Vec::new();
    let mut html = String::with_capacity(input.len());
    let mut code_buf = String::new();
    let mut code_lang = String::new();
    let mut in_code = false;
    let mut in_table_head = false;

    for ev in parser {
        if in_code {
            match ev {
                Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => code_buf.push_str(&t),
                Event::SoftBreak | Event::HardBreak => code_buf.push('\n'),
                Event::End(TagEnd::CodeBlock) => {
                    flush_html(&mut parts, &mut html);
                    parts.push(MdPart::Code {
                        lang: std::mem::take(&mut code_lang),
                        code: std::mem::take(&mut code_buf),
                    });
                    in_code = false;
                }
                _ => {}
            }
            continue;
        }

        match ev {
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code = true;
                code_buf.clear();
                code_lang = match kind {
                    CodeBlockKind::Fenced(lang) => {
                        let lang = lang.trim().to_string();
                        if lang
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                        {
                            lang
                        } else {
                            String::new()
                        }
                    }
                    CodeBlockKind::Indented => String::new(),
                };
            }
            Event::Start(tag) => start_tag(&mut html, &tag, &mut in_table_head),
            Event::End(tag) => end_tag(&mut html, &tag, &mut in_table_head),
            Event::Text(t) => push_esc(&mut html, &t),
            Event::Code(t) => {
                html.push_str("<code>");
                push_esc(&mut html, &t);
                html.push_str("</code>");
            }
            Event::Html(t) | Event::InlineHtml(t) => push_esc(&mut html, &t),
            Event::SoftBreak => html.push('\n'),
            Event::HardBreak => html.push_str("<br>"),
            Event::Rule => html.push_str("<hr>"),
            Event::TaskListMarker(checked) => {
                html.push_str("<input type=\"checkbox\" disabled");
                if checked {
                    html.push_str(" checked");
                }
                html.push('>');
            }
            Event::FootnoteReference(name) => {
                html.push_str("<sup>");
                push_esc(&mut html, &name);
                html.push_str("</sup>");
            }
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                html.push_str("<code>");
                push_esc(&mut html, &t);
                html.push_str("</code>");
            }
        }
    }
    flush_html(&mut parts, &mut html);
    parts
}

fn flush_html(parts: &mut Vec<MdPart>, html: &mut String) {
    if html.chars().any(|c| !c.is_whitespace()) {
        parts.push(MdPart::Html(std::mem::take(html)));
    } else {
        html.clear();
    }
}

fn start_tag(out: &mut String, tag: &Tag<'_>, in_table_head: &mut bool) {
    match tag {
        Tag::Paragraph => out.push_str("<p>"),
        Tag::Heading { level, .. } => {
            out.push('<');
            out.push_str(heading(level));
            out.push('>');
        }
        Tag::BlockQuote(_) => out.push_str("<blockquote>"),
        Tag::CodeBlock(_) => {}
        Tag::List(None) => out.push_str("<ul>"),
        Tag::List(Some(_)) => out.push_str("<ol>"),
        Tag::Item => out.push_str("<li>"),
        Tag::Emphasis => out.push_str("<em>"),
        Tag::Strong => out.push_str("<strong>"),
        Tag::Strikethrough => out.push_str("<del>"),
        Tag::Link { dest_url, .. } => {
            out.push_str("<a");
            if let Some(href) = safe_href(dest_url) {
                out.push_str(" href=\"");
                push_esc(out, &href);
                out.push_str("\" target=\"_blank\" rel=\"noopener noreferrer\"");
            }
            out.push('>');
        }
        Tag::Image {
            dest_url, title, ..
        } => {
            if let Some(href) = safe_href(dest_url) {
                out.push_str("<img src=\"");
                push_esc(out, &href);
                out.push_str("\" alt=\"");
                push_esc(out, title);
                out.push_str("\">");
            }
        }
        Tag::Table(_) => out.push_str("<div class=\"chat-md-table\"><table>"),
        Tag::TableHead => {
            *in_table_head = true;
            out.push_str("<thead><tr>");
        }
        Tag::TableRow => out.push_str("<tr>"),
        Tag::TableCell => {
            if *in_table_head {
                out.push_str("<th>");
            } else {
                out.push_str("<td>");
            }
        }
        Tag::HtmlBlock => {}
        Tag::FootnoteDefinition(_) => out.push_str("<aside>"),
        Tag::DefinitionList => out.push_str("<dl>"),
        Tag::DefinitionListTitle => out.push_str("<dt>"),
        Tag::DefinitionListDefinition => out.push_str("<dd>"),
        Tag::MetadataBlock(_) => {}
        Tag::Superscript => out.push_str("<sup>"),
        Tag::Subscript => out.push_str("<sub>"),
    }
}

fn end_tag(out: &mut String, tag: &TagEnd, in_table_head: &mut bool) {
    match tag {
        TagEnd::Paragraph => out.push_str("</p>"),
        TagEnd::Heading(level) => {
            out.push_str("</");
            out.push_str(heading(level));
            out.push('>');
        }
        TagEnd::BlockQuote(_) => out.push_str("</blockquote>"),
        TagEnd::CodeBlock => {}
        TagEnd::List(false) => out.push_str("</ul>"),
        TagEnd::List(true) => out.push_str("</ol>"),
        TagEnd::Item => out.push_str("</li>"),
        TagEnd::Emphasis => out.push_str("</em>"),
        TagEnd::Strong => out.push_str("</strong>"),
        TagEnd::Strikethrough => out.push_str("</del>"),
        TagEnd::Link => out.push_str("</a>"),
        TagEnd::Image => {}
        TagEnd::Table => out.push_str("</tbody></table></div>"),
        TagEnd::TableHead => {
            *in_table_head = false;
            out.push_str("</tr></thead><tbody>");
        }
        TagEnd::TableRow => out.push_str("</tr>"),
        TagEnd::TableCell => {
            if *in_table_head {
                out.push_str("</th>");
            } else {
                out.push_str("</td>");
            }
        }
        TagEnd::HtmlBlock => {}
        TagEnd::FootnoteDefinition => out.push_str("</aside>"),
        TagEnd::DefinitionList => out.push_str("</dl>"),
        TagEnd::DefinitionListTitle => out.push_str("</dt>"),
        TagEnd::DefinitionListDefinition => out.push_str("</dd>"),
        TagEnd::MetadataBlock(_) => {}
        TagEnd::Superscript => out.push_str("</sup>"),
        TagEnd::Subscript => out.push_str("</sub>"),
    }
}

fn heading(level: &HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "h1",
        HeadingLevel::H2 => "h2",
        HeadingLevel::H3 => "h3",
        HeadingLevel::H4 => "h4",
        HeadingLevel::H5 => "h5",
        HeadingLevel::H6 => "h6",
    }
}

fn safe_href(url: &CowStr<'_>) -> Option<String> {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    if lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:")
        || lower.starts_with('#')
    {
        Some(u.to_string())
    } else {
        None
    }
}

fn push_esc(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
}

// ── syntax highlight (small, WASM-friendly) ────────────────────────────────

const PY: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "None", "not", "or", "pass", "raise", "return", "True", "try", "while", "with",
    "yield",
];
const RS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];
const JS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "of",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];
const SH: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac", "in",
    "function", "return", "export", "local", "readonly", "unset", "shift", "echo", "exit", "set",
];

pub fn highlight(lang: &str, src: &str) -> String {
    let l = lang.to_ascii_lowercase();
    match l.as_str() {
        "json" | "jsonc" => highlight_json(src),
        "py" | "python" => highlight_code(src, PY, "#"),
        "rs" | "rust" => highlight_code(src, RS, "//"),
        "js" | "ts" | "javascript" | "typescript" => highlight_code(src, JS, "//"),
        "bash" | "sh" | "shell" | "zsh" => highlight_code(src, SH, "#"),
        "toml" => highlight_code(src, &[], "#"),
        "yaml" | "yml" => highlight_code(src, &[], "#"),
        _ => {
            let mut o = String::new();
            push_esc(&mut o, src);
            o
        }
    }
}

fn highlight_json(src: &str) -> String {
    let mut out = String::with_capacity(src.len() * 2);
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c == '"' {
            let start = i;
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 2;
                    continue;
                }
                if b[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push_str("<span class=\"tok-str\">");
            push_esc(&mut out, &src[start..i]);
            out.push_str("</span>");
            continue;
        }
        if c.is_ascii_digit()
            || (c == '-' && i + 1 < b.len() && (b[i + 1] as char).is_ascii_digit())
        {
            let start = i;
            i += 1;
            while i < b.len() {
                let ch = b[i] as char;
                if ch.is_ascii_digit()
                    || ch == '.'
                    || ch == 'e'
                    || ch == 'E'
                    || ch == '+'
                    || ch == '-'
                {
                    i += 1;
                } else {
                    break;
                }
            }
            out.push_str("<span class=\"tok-num\">");
            push_esc(&mut out, &src[start..i]);
            out.push_str("</span>");
            continue;
        }
        if c.is_ascii_alphabetic() {
            let start = i;
            while i < b.len() && (b[i] as char).is_ascii_alphanumeric() {
                i += 1;
            }
            let word = &src[start..i];
            if matches!(word, "true" | "false" | "null") {
                out.push_str("<span class=\"tok-kw\">");
                push_esc(&mut out, word);
                out.push_str("</span>");
            } else {
                push_esc(&mut out, word);
            }
            continue;
        }
        push_esc(&mut out, &c.to_string());
        i += 1;
    }
    out
}

fn highlight_code(src: &str, kws: &[&str], line_comment: &str) -> String {
    let mut out = String::with_capacity(src.len() * 2);
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // line comment
        if !line_comment.is_empty() && src[char_byte_idx(&chars, i)..].starts_with(line_comment) {
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            out.push_str("<span class=\"tok-com\">");
            push_esc(&mut out, &chars[start..i].iter().collect::<String>());
            out.push_str("</span>");
            continue;
        }
        let c = chars[i];
        if c == '"' || c == '\'' {
            let q = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                if chars[i] == q {
                    i += 1;
                    break;
                }
                if chars[i] == '\n' {
                    break;
                }
                i += 1;
            }
            out.push_str("<span class=\"tok-str\">");
            push_esc(&mut out, &chars[start..i].iter().collect::<String>());
            out.push_str("</span>");
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                i += 1;
            }
            out.push_str("<span class=\"tok-num\">");
            push_esc(&mut out, &chars[start..i].iter().collect::<String>());
            out.push_str("</span>");
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if kws.iter().any(|k| *k == word) {
                out.push_str("<span class=\"tok-kw\">");
                push_esc(&mut out, &word);
                out.push_str("</span>");
            } else {
                push_esc(&mut out, &word);
            }
            continue;
        }
        push_esc(&mut out, &c.to_string());
        i += 1;
    }
    out
}

fn char_byte_idx(chars: &[char], char_i: usize) -> usize {
    chars.iter().take(char_i).map(|c| c.len_utf8()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_headings_lists_and_code() {
        let parts = markdown_to_parts("# Title\n\n`code` and **bold**\n");
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, MdPart::Html(h) if h.contains("<h1>Title</h1>")))
        );
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, MdPart::Html(h) if h.contains("<code>code</code>")))
        );
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, MdPart::Html(h) if h.contains("<strong>bold</strong>")))
        );
    }

    #[test]
    fn fenced_rs_is_code_part_with_tok_kw_on_let() {
        let parts = markdown_to_parts("```rs\nlet x = 1;\n```");
        let code = parts.iter().find_map(|p| match p {
            MdPart::Code { lang, code } if lang == "rs" => Some(code.as_str()),
            _ => None,
        });
        let code = code.expect("rs code part");
        let hi = highlight("rs", code);
        assert!(hi.contains("<span class=\"tok-kw\">let</span>"), "{hi}");
    }

    #[test]
    fn unordered_list_emits_li_for_each_item() {
        let parts = markdown_to_parts("- one\n- two");
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, MdPart::Html(h) if h.contains("<ul>")))
        );
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, MdPart::Html(h) if h.contains("<li>one</li>")))
        );
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, MdPart::Html(h) if h.contains("<li>two</li>")))
        );
    }

    #[test]
    fn ordered_list_renders_ol() {
        let parts = markdown_to_parts("1. a\n2. b");
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, MdPart::Html(h) if h.contains("<ol>")))
        );
        assert!(
            parts
                .iter()
                .any(|p| matches!(p, MdPart::Html(h) if h.contains("<li>a</li>")))
        );
    }

    #[test]
    fn does_not_emit_raw_html_from_user_text() {
        let parts = markdown_to_parts(
            "Hello <script>alert(1)</script>\n\n<SCRIPT src=//evil.test></SCRIPT>\n\n\
             <img src=x onerror=alert(1)>\n\n<iframe src=//evil.test></iframe>\n\n<svg onload=alert(1)>",
        );
        let html = parts
            .iter()
            .find_map(|p| match p {
                MdPart::Html(h) => Some(h.as_str()),
                _ => None,
            })
            .expect("html part");
        assert!(html.contains("&lt;script"), "{html}");
        assert!(html.contains("&lt;SCRIPT"), "{html}");
        assert!(html.contains("&lt;img"), "{html}");
        assert!(html.contains("&lt;iframe"), "{html}");
        assert!(html.contains("&lt;svg"), "{html}");
        let lower = html.to_ascii_lowercase();
        assert!(!lower.contains("<script"), "{html}");
        assert!(!lower.contains("<img"), "{html}");
        assert!(!lower.contains("<iframe"), "{html}");
        assert!(!lower.contains("<svg"), "{html}");
    }

    #[test]
    fn javascript_and_data_links_have_no_href() {
        let parts = markdown_to_parts(
            "[x](javascript:alert(1))\n\n[y](JAVASCRIPT:alert(1))\n\n\
             ![z](javascript:alert(1))\n\n[d](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)",
        );
        let html = parts
            .iter()
            .find_map(|p| match p {
                MdPart::Html(h) => Some(h.as_str()),
                _ => None,
            })
            .expect("html part");
        assert!(html.contains("<a>"), "{html}");
        let lower = html.to_ascii_lowercase();
        assert!(!lower.contains("href=\"javascript:"), "{html}");
        assert!(!lower.contains("href='javascript:"), "{html}");
        assert!(!lower.contains("src=\"javascript:"), "{html}");
        assert!(!lower.contains("src='javascript:"), "{html}");
        assert!(!lower.contains("href=\"data:"), "{html}");
        assert!(!lower.contains("href='data:"), "{html}");
        assert!(!lower.contains("javascript:"), "{html}");
    }

    #[test]
    fn href_quote_break_is_escaped() {
        let parts = markdown_to_parts("[x](https://example.test/a\" onclick=\"alert(1))");
        let html = parts
            .iter()
            .find_map(|p| match p {
                MdPart::Html(h) => Some(h.as_str()),
                _ => None,
            })
            .expect("html part");
        assert!(html.contains("&quot;"), "{html}");
        assert!(!html.contains("href=\"https://example.test/a\""), "{html}");
    }

    #[test]
    fn protocol_relative_file_and_vbscript_links_have_no_href() {
        let parts = markdown_to_parts(
            "[x](//evil.test) [y](file:///etc/passwd) [z](vbscript:alert(1))",
        );
        let html = parts
            .iter()
            .find_map(|p| match p {
                MdPart::Html(h) => Some(h.as_str()),
                _ => None,
            })
            .expect("html part");
        assert!(html.contains("<a>"), "{html}");
        let lower = html.to_ascii_lowercase();
        assert!(!lower.contains("href=\"//"), "{html}");
        assert!(!lower.contains("href='//"), "{html}");
        assert!(!lower.contains("href=\"file:"), "{html}");
        assert!(!lower.contains("href='file:"), "{html}");
        assert!(!lower.contains("href=\"vbscript:"), "{html}");
        assert!(!lower.contains("href='vbscript:"), "{html}");
        assert!(!lower.contains("src=\"//"), "{html}");
        assert!(!lower.contains("src=\"file:"), "{html}");
        assert!(!lower.contains("src=\"vbscript:"), "{html}");
    }

    #[test]
    fn http_https_mailto_and_hash_links_keep_href() {
        let parts = markdown_to_parts(
            "[a](https://example.test) [b](http://example.test) [c](mailto:a@example.test) [d](#frag)",
        );
        let html = parts
            .iter()
            .find_map(|p| match p {
                MdPart::Html(h) => Some(h.as_str()),
                _ => None,
            })
            .expect("html part");
        assert!(html.contains("href=\"https://example.test\""), "{html}");
        assert!(html.contains("href=\"http://example.test\""), "{html}");
        assert!(html.contains("href=\"mailto:a@example.test\""), "{html}");
        assert!(html.contains("href=\"#frag\""), "{html}");
    }

    #[test]
    fn highlight_unknown_lang_escapes_raw_tags() {
        let hi = highlight("html", "<script>alert(1)</script>");
        assert!(hi.contains("&lt;script"), "{hi}");
        assert!(!hi.contains("<script"), "{hi}");
    }

    #[test]
    fn parts_split_code_blocks() {
        let parts = markdown_to_parts("hi\n\n```json\n{\"a\": 1}\n```\n\nok");
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert_eq!(parts[0], MdPart::Html("<p>hi</p>".into()), "{parts:?}");
        match &parts[1] {
            MdPart::Code { lang, code } => {
                assert_eq!(lang, "json", "{parts:?}");
                assert!(
                    code.contains("{\"a\": 1}"),
                    "code must contain {{\"a\": 1}}: {parts:?}"
                );
            }
            other => panic!("expected Code {{ lang: json, .. }}, got {other:?}"),
        }
        assert_eq!(parts[2], MdPart::Html("<p>ok</p>".into()), "{parts:?}");
    }

    #[test]
    fn fence_info_string_non_ident_is_empty_lang() {
        let parts = markdown_to_parts("```json;alert(1)\n{}\n```");
        match parts.as_slice() {
            [MdPart::Code { lang, code }] => {
                assert_eq!(lang, "", "{parts:?}");
                assert!(code.contains("{}"), "{parts:?}");
            }
            _ => panic!("expected one Code with empty lang, got {parts:?}"),
        }
    }

    #[test]
    fn indented_code_block_is_code_part_empty_lang() {
        let parts = markdown_to_parts("    x = 1\n");
        match parts.as_slice() {
            [MdPart::Code { lang, code }] => {
                assert_eq!(lang, "", "{parts:?}");
                assert!(code.contains("x = 1"), "{parts:?}");
            }
            _ => panic!("expected one indented Code, got {parts:?}"),
        }
    }

    #[test]
    fn consecutive_fences_are_two_code_parts() {
        let parts = markdown_to_parts("```rs\na\n```\n```json\n{}\n```");
        match parts.as_slice() {
            [MdPart::Code {
                lang: rs,
                code: rs_code,
            }, MdPart::Code {
                lang: json,
                code: json_code,
            }] => {
                assert_eq!(rs, "rs", "{parts:?}");
                assert!(rs_code.contains('a'), "{parts:?}");
                assert_eq!(json, "json", "{parts:?}");
                assert!(json_code.contains("{}"), "{parts:?}");
            }
            _ => panic!("expected [Code rs, Code json], got {parts:?}"),
        }
    }

    #[test]
    fn json_highlight_strings_and_nums() {
        let h = highlight("json", r#"{"n": 4}"#);
        assert!(
            h.contains("<span class=\"tok-str\">&quot;n&quot;</span>"),
            "{h}"
        );
        assert!(h.contains("<span class=\"tok-num\">4</span>"), "{h}");
    }

    #[test]
    fn jsonc_alias_highlights_like_json() {
        let h = highlight("jsonc", r#"{"n": 4}"#);
        assert!(
            h.contains("<span class=\"tok-str\">&quot;n&quot;</span>"),
            "{h}"
        );
        assert!(h.contains("<span class=\"tok-num\">4</span>"), "{h}");
    }

    #[test]
    fn json_highlight_keywords_true_false_null() {
        let h = highlight("json", "true false null");
        assert!(h.contains("<span class=\"tok-kw\">true</span>"), "{h}");
        assert!(h.contains("<span class=\"tok-kw\">false</span>"), "{h}");
        assert!(h.contains("<span class=\"tok-kw\">null</span>"), "{h}");
    }

    #[test]
    fn json_highlight_escapes_html_in_strings() {
        let h = highlight("json", r#""<script>""#);
        assert!(
            h.contains("<span class=\"tok-str\">&quot;&lt;script&gt;&quot;</span>"),
            "{h}"
        );
        assert!(!h.contains("<script"), "{h}");
    }
}
