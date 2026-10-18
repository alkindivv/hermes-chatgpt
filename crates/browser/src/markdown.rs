//! Native counterpart of upstream markdown.ts and browser-worker chatGptMarkdownContent (MIT).
//! Complete delivery replaces drafts; snapshots are never append-only deltas.
use crate::{Error, Result};
use regex::Regex;
use scraper::{ElementRef, Html, Selector};

#[derive(Debug, Clone, thiserror::Error)]
#[error("assistant Markdown conversion failed: {reason}; raw evidence retained")]
pub struct MarkdownError {
    pub reason: String,
    pub html: String,
    pub text_fallback: String,
}
struct Renderer<'a> {
    html: &'a str,
    fallback: &'a str,
}
fn selector(value: &str) -> Selector {
    Selector::parse(value).expect("static browser selector")
}
fn class(element: ElementRef<'_>, name: &str) -> bool {
    element.value().classes().any(|c| c == name)
}
fn text(element: ElementRef<'_>) -> String {
    element.text().collect()
}
fn re(pattern: &'static str) -> Regex {
    static CACHE: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<&'static str, Regex>>,
    > = std::sync::LazyLock::new(Default::default);
    CACHE
        .lock()
        .unwrap()
        .entry(pattern)
        .or_insert_with(|| Regex::new(pattern).expect("static Markdown pattern"))
        .clone()
}
fn escape(value: &str) -> String {
    let mut value = value
        .replace('\\', "\\\\")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('`', "\\`");
    for pattern in [
        r"(?m)^(\s*)(#{1,6} )",
        r"(?m)^(\s*)(- )",
        r"(?m)^(\s*)(\+ )",
        r"(?m)^(\s*)(> )",
    ] {
        value = re(pattern).replace_all(&value, "$1\\$2").into_owned();
    }
    re(r"(?m)^(\s*\d+)\. ")
        .replace_all(&value, "$1\\. ")
        .into_owned()
}
fn normalize(value: &str) -> String {
    re(r"[\t\r\n ]+").replace_all(value, " ").into_owned()
}
fn block(value: String) -> String {
    if value.trim().is_empty() {
        String::new()
    } else {
        format!("\n\n{}\n\n", value.trim_matches('\n'))
    }
}
fn decorate(content: &str, open: &str, close: &str) -> String {
    if content.trim().is_empty() {
        return content.into();
    }
    let leading = &content[..content.len() - content.trim_start().len()];
    let trailing = &content[content.trim_end().len()..];
    format!("{leading}{open}{}{close}{trailing}", content.trim())
}
fn join(left: &mut String, right: &str) {
    if left.ends_with('\n') && right.starts_with('\n') {
        let a = left.chars().rev().take_while(|c| *c == '\n').count();
        let b = right.chars().take_while(|c| *c == '\n').count();
        left.truncate(left.len() - a);
        left.push_str(&"\n".repeat(a.max(b).min(2)));
        left.push_str(&right[b..]);
    } else {
        left.push_str(right)
    }
}
impl Renderer<'_> {
    fn fail<T>(&self, reason: impl Into<String>) -> Result<T> {
        Err(MarkdownError {
            reason: reason.into(),
            html: self.html.into(),
            text_fallback: self.fallback.into(),
        }
        .into())
    }
    fn children(&self, element: ElementRef<'_>, anchor: bool) -> Result<String> {
        let mut result = String::new();
        for node in element.children() {
            if let Some(child) = ElementRef::wrap(node) {
                join(&mut result, &self.element(child, anchor)?)
            } else if let Some(value) = node.value().as_text() {
                let content = normalize(&value.text);
                if !content.trim().is_empty()
                    || (!result.is_empty() && !result.ends_with(['\n', ' ']))
                {
                    let content = if result.ends_with(' ') {
                        content.strip_prefix(' ').unwrap_or(&content)
                    } else {
                        &content
                    };
                    result.push_str(&escape(content));
                }
            }
        }
        Ok(result)
    }
    fn element(&self, e: ElementRef<'_>, anchor: bool) -> Result<String> {
        let tag = e.value().name();
        // Observed renderer chrome and non-text widgets are intentionally excluded upstream.
        if matches!(
            tag,
            "script" | "style" | "button" | "svg" | "img" | "picture" | "source"
        ) {
            if tag == "button" && class(e, "behavior-btn") && class(e, "entity-underline") {
                return self.children(e, anchor);
            }
            return Ok(String::new());
        }
        if e.value().attr("hidden").is_some()
            || e.value().attr("aria-hidden") == Some("true")
            || class(e, "sr-only")
            || class(e, "chart-widget-container")
            || e.value().attr("data-code-block-preview-pane").is_some()
            || e.value().attr("data-markdown-copy") == Some("exclude")
        {
            return Ok(String::new());
        }
        if class(e, "katex") {
            let sources: Vec<_> = e
                .select(&selector("annotation[encoding=\"application/x-tex\"]"))
                .collect();
            if sources.len() != 1 {
                return self.fail("formula does not contain one unambiguous LaTeX source");
            }
            let source = text(sources[0]);
            return Ok(
                if e.parent()
                    .and_then(ElementRef::wrap)
                    .is_some_and(|p| class(p, "katex-display"))
                {
                    block(format!("\\[\n{source}\n\\]"))
                } else {
                    format!("\\({source}\\)")
                },
            );
        }
        if e.value().attr("data-markdown-copy") == Some("rich-block") {
            let bodies: Vec<_> = e
                .select(&selector("[data-markdown-copy-content=\"true\"]"))
                .filter(|b| {
                    b.ancestors()
                        .filter_map(ElementRef::wrap)
                        .find(|p| p.value().attr("data-markdown-copy") == Some("rich-block"))
                        .is_some_and(|p| p.id() == e.id())
                })
                .collect();
            if bodies.len() != 1 {
                return self.fail("writing card does not expose one unambiguous content body");
            }
            return self.children(bodies[0], anchor);
        }
        if e.value().attr("data-chatgpt-copy-reference").is_some()
            && e.value().attr("data-markdown-copy") == Some("contents")
            && e.select(&selector("[class~=\"group/resource-row\"]"))
                .next()
                .is_some()
            && e.select(&selector("a[href]")).next().is_none()
        {
            return Ok(String::new());
        }
        if tag == "pre" || e.value().attr("data-markdown-copy") == Some("code-block") {
            return self.code_block(e);
        }
        match tag {
            "html" | "body" | "span" | "small" | "label" => self.children(e, anchor),
            "div" | "section" | "article" | "main" | "figure" | "figcaption" => {
                Ok(block(self.children(e, anchor)?))
            }
            "p" => {
                let value = self.children(e, anchor)?;
                let value = re(r"(?m)^([ \t]*)<(\/?)proposed\\_plan>([ \t]*)$")
                    .replace_all(&value, "${1}<${2}proposed_plan>${3}")
                    .into_owned();
                Ok(block(value))
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Ok(block(format!(
                "{} {}",
                "#".repeat(tag[1..].parse().unwrap_or(1)),
                self.children(e, anchor)?.trim()
            ))),
            "strong" | "b" => Ok(decorate(&self.children(e, anchor)?, "**", "**")),
            "em" | "i" => Ok(decorate(&self.children(e, anchor)?, "*", "*")),
            "del" | "s" | "strike" => Ok(decorate(&self.children(e, anchor)?, "~", "~")),
            "br" => Ok("  \n".into()),
            "hr" => Ok(block("* * *".into())),
            "blockquote" => Ok(block(
                self.children(e, anchor)?
                    .trim()
                    .lines()
                    .map(|line| {
                        if line.is_empty() {
                            ">".into()
                        } else {
                            format!("> {line}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            "code" => {
                let value = text(e);
                if !anchor && file_path(&value) {
                    return Ok(format!(
                        "[{}](<{}>)",
                        escape(&value),
                        value.replace('\\', "/")
                    ));
                }
                let value = value.replace('\n', " ");
                let longest = value.split(|c| c != '`').map(str::len).max().unwrap_or(0);
                let ticks = "`".repeat(longest + 1);
                let padding = if value.starts_with('`')
                    || value.ends_with('`')
                    || (value.starts_with(' ') && value.ends_with(' ') && !value.trim().is_empty())
                {
                    " "
                } else {
                    ""
                };
                Ok(format!("{ticks}{padding}{value}{padding}{ticks}"))
            }
            "a" => {
                let content = self.children(e, true)?;
                let Some(href) = e.value().attr("href") else {
                    return Ok(content);
                };
                if href.trim().is_empty() {
                    return self.fail("link destination is empty");
                }
                let href = href.replace('(', "\\(").replace(')', "\\)");
                let title = e
                    .value()
                    .attr("title")
                    .map(|t| format!(" \"{}\"", t.replace('"', "\\\"")))
                    .unwrap_or_default();
                Ok(decorate(&content, "[", &format!("]({href}{title})")))
            }
            "ul" | "ol" => self.list(e, anchor),
            "table" => self.table(e),
            "input" if e.value().attr("type") == Some("checkbox") => {
                Ok(if e.value().attr("checked").is_some() {
                    "[x] ".into()
                } else {
                    "[ ] ".into()
                })
            }
            "li" => self.children(e, anchor),
            // Never silently erase semantics of an unknown rich renderer.
            _ => self.fail(format!("unsupported assistant element <{tag}>")),
        }
    }
    fn code_block(&self, e: ElementRef<'_>) -> Result<String> {
        let codes: Vec<_> = e.select(&selector("code")).collect();
        let (source, language) = if codes.len() == 1 {
            let code = codes[0];
            let language = code
                .value()
                .classes()
                .find_map(|c| c.strip_prefix("language-"))
                .or(code.value().attr("data-language"))
                .unwrap_or("");
            (text(code), language.to_string())
        } else if codes.is_empty() {
            let editors: Vec<_> = e.select(&selector(".cm-content[data-language]")).collect();
            if editors.len() != 1 {
                return self.fail("code block has no unique code source");
            }
            let editor = editors[0];
            let lines: Vec<_> = editor.children().filter_map(ElementRef::wrap).collect();
            if lines.iter().any(|line| !class(*line, "cm-line")) {
                return self.fail("CodeMirror block has incomplete logical lines");
            }
            (
                lines.into_iter().map(text).collect::<Vec<_>>().join("\n"),
                editor
                    .value()
                    .attr("data-language")
                    .unwrap_or("")
                    .to_string(),
            )
        } else {
            return self.fail("code block has ambiguous code sources");
        };
        if language.contains(['\n', '\r', '`']) {
            return self.fail("code language cannot be represented as a fence label");
        }
        let longest = source.split(|c| c != '`').map(str::len).max().unwrap_or(0);
        let fence = "`".repeat(3.max(longest + 1));
        let source = source.strip_suffix('\n').unwrap_or(&source);
        Ok(block(format!("{fence}{language}\n{source}\n{fence}")))
    }
    fn list(&self, e: ElementRef<'_>, anchor: bool) -> Result<String> {
        let start = e
            .value()
            .attr("start")
            .unwrap_or("1")
            .parse::<i64>()
            .map_err(|_| MarkdownError {
                reason: "invalid ordered list start".into(),
                html: self.html.into(),
                text_fallback: self.fallback.into(),
            })?;
        let mut items = Vec::new();
        for (i, li) in e.children().filter_map(ElementRef::wrap).enumerate() {
            if li.value().name() != "li" {
                return self.fail("list contains a non-item element");
            }
            let prefix = if e.value().name() == "ol" {
                format!("{}. ", start + i as i64)
            } else {
                "- ".into()
            };
            let content = self.children(li, anchor)?;
            let content = content.trim_matches('\n');
            items.push(format!(
                "{prefix}{}",
                content.replace('\n', &format!("\n{}", " ".repeat(prefix.len())))
            ));
        }
        if e.parent().and_then(ElementRef::wrap).is_some_and(|parent| {
            parent.value().name() == "li"
                && parent
                    .children()
                    .filter_map(ElementRef::wrap)
                    .last()
                    .is_some_and(|last| last.id() == e.id())
        }) {
            Ok(format!("\n{}", items.join("\n")))
        } else {
            Ok(block(items.join("\n")))
        }
    }
    fn table(&self, e: ElementRef<'_>) -> Result<String> {
        let rows: Vec<_> = e.select(&selector("tr")).collect();
        let mut output = Vec::new();
        let mut width = None;
        if rows.is_empty() {
            return self.fail("empty table");
        }
        for (i, row) in rows.iter().enumerate() {
            let cells: Vec<_> = row.children().filter_map(ElementRef::wrap).collect();
            if cells.is_empty()
                || cells.iter().any(|c| {
                    !matches!(c.value().name(), "th" | "td")
                        || c.value().attr("rowspan").is_some_and(|v| v != "1")
                        || c.value().attr("colspan").is_some_and(|v| v != "1")
                })
            {
                return self.fail("table contains unsupported cell spans or structure");
            }
            if i == 0 && !cells.iter().all(|c| c.value().name() == "th") {
                return self.fail("table has no unambiguous header row");
            }
            if width.is_some_and(|w| w != cells.len()) {
                return self.fail("table rows have inconsistent widths");
            }
            width = Some(cells.len());
            let mut rendered = Vec::new();
            for cell in &cells {
                let content = self.children(*cell, false)?;
                rendered.push(content.trim().replace('|', "\\|").replace('\n', "<br>"));
            }
            output.push(format!("| {} |", rendered.join(" | ")));
            if i == 0 {
                let rules = cells
                    .iter()
                    .map(|c| match c.value().attr("align") {
                        Some("left") => ":--",
                        Some("right") => "--:",
                        Some("center") => ":-:",
                        _ => "---",
                    })
                    .collect::<Vec<_>>();
                output.push(format!("| {} |", rules.join(" | ")));
            }
        }
        Ok(block(output.join("\n")))
    }
}
fn file_path(value: &str) -> bool {
    if value.trim() != value
        || value
            .chars()
            .any(|c| c.is_whitespace() || "`<>[]()".contains(c))
        || re(r"(?i)^[a-z][a-z\d+.-]*://").is_match(value)
    {
        return false;
    }
    let path = re(r":\d+(?::\d+)?$").replace(value, "");
    let Some(basename) = path.rsplit_once(['/', '\\']).map(|(_, base)| base) else {
        return false;
    };
    re(r"(?i)\.[a-z\d][a-z\d._-]*$").is_match(basename)
}
/// HTML is authoritative. Only genuinely absent HTML may use the explicit text fallback.
/// Unsupported rich content returns Error::Markdown with both original inputs retained.
pub fn render_assistant_markdown(html: &str, text_fallback: &str) -> Result<String> {
    if html.trim().is_empty() {
        return Ok(text_fallback.to_string());
    }
    let document = Html::parse_fragment(html);
    let renderer = Renderer {
        html,
        fallback: text_fallback,
    };
    let output = renderer.element(document.root_element(), false)?;
    let output = link_wiki(&output);
    if output.trim().is_empty() && !text_fallback.trim().is_empty() {
        return renderer.fail("HTML produced no answer content; refusing to flatten rich fallback");
    }
    Ok(output.trim().to_string())
}
fn link_wiki(markdown: &str) -> String {
    // Code and math are opaque; wiki-shaped TeX/code is never rewritten.
    let mut result = String::new();
    let mut offset = 0;
    let mut ticks = 0;
    let mut math_end = None;
    while offset < markdown.len() {
        let tail = &markdown[offset..];
        if let Some(end) = math_end {
            if tail.starts_with(end) {
                result.push_str(end);
                offset += 2;
                math_end = None;
            } else {
                let c = tail.chars().next().unwrap();
                result.push(c);
                offset += c.len_utf8();
            }
            continue;
        }
        if tail.starts_with('`') {
            let n = tail.chars().take_while(|c| *c == '`').count();
            ticks = if ticks == 0 {
                n
            } else if ticks == n {
                0
            } else {
                ticks
            };
            result.push_str(&tail[..n]);
            offset += n;
            continue;
        }
        if ticks == 0
            && (tail.starts_with("\\(") || tail.starts_with("\\["))
            && !tail.starts_with("\\[\\[")
        {
            let end = if tail.starts_with("\\(") {
                "\\)"
            } else {
                "\\]"
            };
            math_end = Some(end);
            result.push_str(&tail[..2]);
            offset += 2;
            continue;
        }
        let escaped = tail.starts_with("\\[\\[");
        if ticks == 0 && (escaped || tail.starts_with("[[")) {
            let (open, close) = if escaped { (4, "\\]\\]") } else { (2, "]]") };
            if let Some(end) = tail[open..].find(close) {
                let value = &tail[open..open + end];
                let (target, label) = value.split_once('|').unwrap_or((value, value));
                let target = target.trim();
                let label = label.trim();
                if !result.ends_with('!')
                    && !target.is_empty()
                    && !label.is_empty()
                    && !target.contains(['<', '>'])
                {
                    let (note, fragment) = target
                        .split_once('#')
                        .map(|(n, f)| (n, format!("#{f}")))
                        .unwrap_or((target, String::new()));
                    let extension = note.rsplit('/').next().unwrap_or("").contains('.');
                    let suffix = if !note.is_empty() && !extension {
                        ".md"
                    } else {
                        ""
                    };
                    result.push_str(&format!("[{label}](<{note}{suffix}{fragment}>)"));
                } else {
                    result.push_str(&format!("[[{value}]]"))
                }
                offset += open + end + close.len();
                continue;
            }
        }
        let c = tail.chars().next().unwrap();
        result.push(c);
        offset += c.len_utf8();
    }
    result
}
/// Upstream's complete-delivery semantics: observe replaces an undelivered draft;
/// finish emits the final coherent snapshot once. There are no provisional text deltas.
#[derive(Default)]
pub struct AtomicMarkdownBuffer {
    latest: Option<(String, String)>,
    finished: Option<String>,
    extent: Option<(u64, u64)>,
    longest: Option<String>,
}
fn source_extent(html: &str) -> Option<(u64, u64)> {
    let doc = Html::parse_fragment(html);
    let range_selector = selector("[data-start][data-end]");
    let ranges = doc.select(&range_selector).filter_map(|e| {
        let start = e.value().attr("data-start")?.parse::<u64>().ok()?;
        let end = e.value().attr("data-end")?.parse::<u64>().ok()?;
        (end >= start).then_some((start, end))
    });
    ranges.reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
}
impl AtomicMarkdownBuffer {
    pub fn observe(&mut self, html: &str, text_fallback: &str) -> Result<()> {
        if self.finished.is_some() {
            return Err(Error::Dom("Markdown delivery has already finished".into()));
        }
        if let Some(range) = source_extent(html) {
            self.extent = Some(
                self.extent
                    .map_or(range, |old| (old.0.min(range.0), old.1.max(range.1))),
            );
        }
        if let Ok(markdown) = render_assistant_markdown(html, text_fallback)
            && self
                .longest
                .as_ref()
                .is_none_or(|old| old.len() < markdown.len())
        {
            self.longest = Some(markdown);
        }
        self.latest = Some((html.into(), text_fallback.into()));
        Ok(())
    }
    pub fn finish(&mut self) -> Result<String> {
        if self.finished.is_some() {
            return Ok(String::new());
        }
        let (html, text) = self
            .latest
            .as_ref()
            .ok_or_else(|| Error::Dom("no final assistant snapshot".into()))?;
        let result = render_assistant_markdown(html, text)?;
        if self.extent.is_some_and(|seen| {
            source_extent(html).is_none_or(|last| last.0 > seen.0 || last.1 < seen.1)
        }) || self.longest.as_ref().is_some_and(|previous| {
            previous.len() > result.len()
                && (previous.starts_with(&result) || previous.ends_with(&result))
        }) {
            return Err(MarkdownError{reason:"final snapshot may have lost an observed prefix or source range; refusing truncated delivery".into(),html:html.clone(),text_fallback:text.clone()}.into());
        }
        self.finished = Some(result.clone());
        Ok(result)
    }
}
