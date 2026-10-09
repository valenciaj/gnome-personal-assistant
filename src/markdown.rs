//! Markdown → marcado Pango, para mostrar respuestas con formato en un `gtk::Label`.
//!
//! Todo el texto se escapa y pulldown-cmark anida bien las etiquetas, así que el
//! resultado siempre es marcado válido.

use gtk::glib::markup_escape_text;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

pub fn to_pango(markdown: &str) -> String {
    let mut out = String::new();
    // Pila de listas: None = viñetas, Some(n) = numerada por n.
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut needs_break = false;

    let block_break = |out: &mut String, needs_break: &mut bool| {
        if *needs_break {
            out.push_str("\n\n");
            *needs_break = false;
        }
    };

    for event in Parser::new_ext(markdown, Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    if lists.is_empty() {
                        block_break(&mut out, &mut needs_break);
                    }
                }
                Tag::Heading { level, .. } => {
                    block_break(&mut out, &mut needs_break);
                    let size = match level {
                        HeadingLevel::H1 => "x-large",
                        HeadingLevel::H2 => "large",
                        _ => "medium",
                    };
                    out.push_str(&format!("<span size=\"{size}\" weight=\"bold\">"));
                }
                Tag::CodeBlock(_) => {
                    block_break(&mut out, &mut needs_break);
                    out.push_str("<tt>");
                }
                Tag::List(start) => {
                    if lists.is_empty() {
                        block_break(&mut out, &mut needs_break);
                    }
                    lists.push(start);
                }
                Tag::Item => {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&"    ".repeat(lists.len().saturating_sub(1)));
                    match lists.last_mut() {
                        Some(Some(n)) => {
                            out.push_str(&format!("{n}. "));
                            *n += 1;
                        }
                        _ => out.push_str("• "),
                    }
                }
                Tag::BlockQuote(_) => {
                    block_break(&mut out, &mut needs_break);
                    out.push_str("<i>");
                }
                Tag::Emphasis => out.push_str("<i>"),
                Tag::Strong => out.push_str("<b>"),
                Tag::Strikethrough => out.push_str("<s>"),
                Tag::Link { dest_url, .. } => {
                    out.push_str(&format!("<a href=\"{}\">", markup_escape_text(&dest_url)));
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => needs_break = lists.is_empty(),
                TagEnd::Heading(_) => {
                    out.push_str("</span>");
                    needs_break = true;
                }
                TagEnd::CodeBlock => {
                    while out.ends_with('\n') {
                        out.pop();
                    }
                    out.push_str("</tt>");
                    needs_break = true;
                }
                TagEnd::List(_) => {
                    lists.pop();
                    needs_break = lists.is_empty();
                }
                TagEnd::BlockQuote(_) => {
                    out.push_str("</i>");
                    needs_break = true;
                }
                TagEnd::Emphasis => out.push_str("</i>"),
                TagEnd::Strong => out.push_str("</b>"),
                TagEnd::Strikethrough => out.push_str("</s>"),
                TagEnd::Link => out.push_str("</a>"),
                _ => {}
            },
            // El HTML no se interpreta: se muestra tal cual.
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                out.push_str(&markup_escape_text(&text))
            }
            Event::Code(code) => out.push_str(&format!("<tt>{}</tt>", markup_escape_text(&code))),
            Event::SoftBreak => out.push(' '),
            Event::HardBreak => out.push('\n'),
            Event::Rule => {
                block_break(&mut out, &mut needs_break);
                out.push_str("──────────");
                needs_break = true;
            }
            Event::TaskListMarker(done) => out.push_str(if done { "☑ " } else { "☐ " }),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::to_pango;

    #[test]
    fn renders_basic_markdown() {
        let pango = to_pango("# Hola\n\nTexto **fuerte** y `código` <x>.\n\n- uno\n- dos\n\n1. a\n2. b");
        assert_eq!(
            pango,
            "<span size=\"x-large\" weight=\"bold\">Hola</span>\n\n\
             Texto <b>fuerte</b> y <tt>código</tt> &lt;x&gt;.\n\n\
             • uno\n• dos\n\n1. a\n2. b"
        );
    }

    #[test]
    fn output_is_valid_pango() {
        let pango = to_pango("*a* ~~b~~ <c>\n\n> cita\n\n```\nfn x() {}\n```\n\n---\n\n1. x\n   - y");
        gtk::pango::parse_markup(&pango, '\0').expect("marcado Pango válido");
    }

    #[test]
    fn escapes_link_targets() {
        // <a> solo lo entiende GtkLabel, no pango::parse_markup.
        assert_eq!(to_pango("[b](http://x?a=1&b=2)"), "<a href=\"http://x?a=1&amp;b=2\">b</a>");
    }
}
