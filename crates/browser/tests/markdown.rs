use cw_browser::{AtomicMarkdownBuffer, Error, render_assistant_markdown as render};
use scraper::{Html, Selector};
fn roots(fixture: &str) -> String {
    let document = Html::parse_fragment(fixture);
    document.select(&Selector::parse(".markdown, [data-markdown-text-style=\"assistant-message\"], [class*=\"_DilResponseRoot\"]").unwrap()).map(|e|e.html()).collect::<Vec<_>>().join("\n")
}
#[test]
fn headings_emphasis_links_and_compact_lists() {
    assert_eq!(render("<h1>Probe</h1><p>Hello <strong>Alex</strong>, <em>yes</em>. <a href=\"https://example.com/\">details</a></p><ul><li>First</li><li>Second</li></ul>","").unwrap(),"# Probe\n\nHello **Alex**, *yes*. [details](https://example.com/)\n\n- First\n- Second");
    assert_eq!(
        render("<ol start=\"3\"><li>Three</li><li>Four</li></ol>", "").unwrap(),
        "3. Three\n4. Four"
    );
}
#[test]
fn fences_preserve_language_indentation_and_embedded_fences() {
    assert_eq!(render("<p>Example</p><pre><code class=\"language-python\">if True:\n  print(1)\n</code></pre>","").unwrap(),"Example\n\n```python\nif True:\n  print(1)\n```");
    assert_eq!(
        render(
            "<pre><code class=\"language-text\">a\n```\nend</code></pre>",
            ""
        )
        .unwrap(),
        "````text\na\n```\nend\n````"
    );
}
#[test]
fn gfm_tables_preserve_alignment_links_and_literal_pipes() {
    let html = "<table><thead><tr><th align=\"left\">Name</th><th align=\"right\">Value</th></tr></thead><tbody><tr><td><a href=\"https://example.com\">A</a></td><td>a|b</td></tr></tbody></table>";
    assert_eq!(
        render(html, "").unwrap(),
        "| Name | Value |\n| :-- | --: |\n| [A](https://example.com) | a\\|b |"
    );
    assert!(
        render(
            "<table><tr><th>A</th></tr><tr><td colspan=\"2\">B</td></tr></table>",
            "B"
        )
        .is_err()
    );
}
#[test]
fn file_paths_and_wiki_links_match_upstream_prose_rules() {
    assert_eq!(render("<p>Created <code>src/_private_/file_name.ts:4</code>. Open [[Notes/weekly-review|review]] and [[Projects/sample#Status]].</p>","").unwrap(),"Created [src/\\_private\\_/file\\_name.ts:4](<src/_private_/file_name.ts:4>). Open [review](<Notes/weekly-review.md>) and [Projects/sample#Status](<Projects/sample.md#Status>).");
    assert_eq!(render("<p><a href=\"https://example.com/source\"><code>src/example.ts</code></a> <code>[[note]]</code> ![[image.png]]</p>","").unwrap(),"[`src/example.ts`](https://example.com/source) `[[note]]` ![[image.png]]");
}
fn katex(source: &str, display: bool) -> String {
    let source = source
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let html = format!(
        "<span class=\"katex\"><span class=\"katex-mathml\"><math><semantics><mi>DUPLICATE</mi><annotation encoding=\"application/x-tex\">{source}</annotation></semantics></math></span><span class=\"katex-html\" aria-hidden=\"true\">VISUAL</span></span>"
    );
    if display {
        format!("<span class=\"katex-display\">{html}</span>")
    } else {
        html
    }
}
#[test]
fn math_keeps_exact_tex_and_comment_newlines_without_duplicate_layers() {
    assert_eq!(
        render(
            &format!(
                "<p>Inline {}.</p>{}",
                katex("E = mc^2", false),
                katex("a % comment\n+ b [[x]]", true)
            ),
            ""
        )
        .unwrap(),
        "Inline \\(E = mc^2\\).\n\n\\[\na % comment\n+ b [[x]]\n\\]"
    );
    assert!(render("<span class=\"katex\">unproven</span>", "unproven").is_err());
    assert!(render("<span class=\"katex\"><annotation encoding=\"application/x-tex\">x</annotation><annotation encoding=\"application/x-tex\">y</annotation></span>","xy").is_err());
}
#[test]
fn writing_card_uses_only_unique_body_and_retains_code() {
    let html = "<div data-markdown-copy=\"rich-block\"><div>Toolbar Title</div><div data-markdown-copy-content=\"true\"><p>Hello <strong>Alex</strong>.</p><p><a href=\"https://example.com/\">details</a></p><pre><code>line 1\n  line 2</code></pre></div><button>Send</button></div>";
    assert_eq!(
        render(html, "").unwrap(),
        "Hello **Alex**.\n\n[details](https://example.com/)\n\n```\nline 1\n  line 2\n```"
    );
    assert!(
        render(
            "<div data-markdown-copy=\"rich-block\"><div>Ambiguous body</div></div>",
            "Ambiguous body"
        )
        .is_err()
    );
}
#[test]
fn code_mirror_uses_logical_lines_and_rejects_incomplete_editor() {
    let html = "<div data-markdown-copy=\"code-block\"><div data-markdown-copy=\"exclude\">Python Copy</div><div class=\"cm-content\" data-language=\"python\"><div class=\"cm-line\">print(1)</div><div class=\"cm-line\">  print(2)</div></div></div>";
    assert_eq!(
        render(html, "").unwrap(),
        "```python\nprint(1)\n  print(2)\n```"
    );
    assert!(
        render(
            &html.replace("class=\"cm-line\"", "class=\"unknown\""),
            "print(1)"
        )
        .is_err()
    );
}
#[test]
fn known_chrome_and_resource_previews_do_not_become_answer_text() {
    let html = "<p>Intro.</p><div class=\"chart-widget-container\">Axis Spinner</div><div data-chatgpt-copy-reference=\"true\" data-markdown-copy=\"contents\"><div class=\"group/resource-row\">Loading file</div></div><p><button class=\"behavior-btn entity-underline\">report.md<span hidden>Hidden</span></button></p><p><a href=\"https://example.com/report\">Download</a></p><button>Follow up</button>";
    assert_eq!(
        render(html, "").unwrap(),
        "Intro.\n\nreport.md\n\n[Download](https://example.com/report)"
    );
}
#[test]
fn unknown_rich_content_retains_raw_evidence_instead_of_text_fallback() {
    for html in [
        "<canvas>lost graph</canvas>",
        "<iframe src=\"https://example.com\"></iframe>",
        "<custom-answer>unknown</custom-answer>",
    ] {
        match render(html, "tempting flattened fallback").unwrap_err() {
            Error::Markdown(error) => {
                assert_eq!(error.html, html);
                assert_eq!(error.text_fallback, "tempting flattened fallback");
                assert!(!error.to_string().contains("tempting flattened"));
            }
            other => panic!("{other}"),
        }
    }
    assert_eq!(
        render("", "explicit plain fallback").unwrap(),
        "explicit plain fallback"
    );
}
#[test]
fn source_plan_markers_keep_semantics_outside_code() {
    assert_eq!(
        render(
            "<p>&lt;proposed_plan&gt;</p><h2>Plan</h2><p>&lt;/proposed_plan&gt;</p>",
            ""
        )
        .unwrap(),
        "<proposed_plan>\n\n## Plan\n\n</proposed_plan>"
    );
}
#[test]
fn captured_codemirror_fixture_matches_upstream_exact_expectation() {
    let fixture = include_str!(
        "../../../vendor/upstream/tests/fixtures/chatgpt-codemirror-reply-2026-10-11.html"
    );
    assert_eq!(
        render(&roots(fixture), "").unwrap(),
        "# Probe\n\n- First item\n- Second item\n\n```python\nprint(1)\n```"
    );
}
#[test]
fn captured_dil_smoke_fixture_preserves_exact_marker() {
    let fixture = include_str!("../../../vendor/upstream/tests/fixtures/chatgpt-dil-smoke.html");
    assert_eq!(render(&roots(fixture), "").unwrap(), "CODEX WEB GPT READY");
}
#[test]
fn captured_power_complete_preserves_headings_list_and_terminal_marker() {
    let fixture =
        include_str!("../../../vendor/upstream/tests/fixtures/chatgpt-power-complete.html");
    let output = render(&roots(fixture), "").unwrap();
    assert!(output.starts_with("## How a Rainbow Begins\n\n"));
    assert!(
        output.contains(
            "1. Sunlight enters the droplet and refracts.\n2. The separated light reflects"
        )
    );
    assert!(output.ends_with("STREAM\\_END\\_927"));
    assert!(!output.contains("Copy"));
}
#[test]
fn complete_delivery_replaces_drafts_and_never_appends_duplicate_snapshots() {
    let mut buffer = AtomicMarkdownBuffer::default();
    buffer.observe("<p>Earlier summary.</p>", "").unwrap();
    buffer
        .observe("<h2>Corrected</h2><p>Final detail.</p>", "")
        .unwrap();
    buffer
        .observe("<h2>Corrected</h2><p>Final detail.</p>", "")
        .unwrap();
    assert_eq!(buffer.finish().unwrap(), "## Corrected\n\nFinal detail.");
    assert_eq!(buffer.finish().unwrap(), "");
    assert!(buffer.observe("<p>Late</p>", "").is_err());
}
#[test]
fn missing_final_prefix_or_source_range_fails_instead_of_truncating() {
    for (before, after) in [
        ("<p>First.</p><p>Last.</p>", "<p>Last.</p>"),
        (
            "<p data-start=\"0\" data-end=\"10\">First.</p><p data-start=\"11\" data-end=\"20\">Last.</p>",
            "<p data-start=\"11\" data-end=\"20\">Changed last.</p>",
        ),
    ] {
        let mut buffer = AtomicMarkdownBuffer::default();
        buffer.observe(before, "").unwrap();
        buffer.observe(after, "").unwrap();
        assert!(matches!(buffer.finish(), Err(Error::Markdown(_))));
    }
}

#[test]
fn inline_flanking_spaces_are_not_lost_and_checkboxes_do_not_duplicate_spaces() {
    assert_eq!(
        render(
            "<p>one<strong> bold </strong>two<a href=\"/link\"> link </a>end</p>",
            ""
        )
        .unwrap(),
        "one **bold** two [link](/link) end"
    );
    assert_eq!(render("<ul><li><input type=\"checkbox\" checked> Done</li><li><input type=\"checkbox\"> Open</li></ul>","").unwrap(),"- [x] Done\n- [ ] Open");
}
#[test]
fn nested_lists_and_paragraph_continuations_match_source_compact_indentation() {
    assert_eq!(
        render("<ul><li>Parent<ul><li>Child</li></ul></li></ul>", "").unwrap(),
        "- Parent\n  - Child"
    );
    assert_eq!(
        render(
            "<ul><li><p>Keep snake_case.</p><p>&lt;/proposed_plan&gt;</p></li></ul>",
            ""
        )
        .unwrap(),
        "- Keep snake\\_case.\n  \n  </proposed_plan>"
    );
}
