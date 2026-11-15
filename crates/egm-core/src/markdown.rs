//! Notion blocks -> Markdown (PRD §5). Supported: headings, paragraphs,
//! lists, to-dos, quotes, code (incl. mermaid), tables, callouts (emoji and
//! colour kept), toggles (as <details>), dividers, table of contents and
//! child-page markers. Anything else becomes a visible placeholder line and
//! a warning; a round with warnings cannot be judged GO.

use serde_json::Value;

use crate::notion::Block;

#[derive(Debug, Default)]
pub struct Rendered {
    pub markdown: String,
    pub warnings: Vec<String>,
}

pub fn render(blocks: &[Block]) -> Rendered {
    let mut r = Renderer::default();
    let mut markdown = r.blocks(blocks);
    if !markdown.is_empty() {
        markdown.push('\n');
    }
    Rendered { markdown, warnings: r.warnings }
}

#[derive(Default)]
struct Renderer {
    warnings: Vec<String>,
}

fn is_list(kind: &str) -> bool {
    matches!(kind, "bulleted_list_item" | "numbered_list_item" | "to_do")
}

impl Renderer {
    fn blocks(&mut self, blocks: &[Block]) -> String {
        let mut out = String::new();
        let mut prev: Option<&str> = None;
        let mut num = 0;
        for b in blocks {
            let kind = b.kind();
            num = if kind == "numbered_list_item" { num + 1 } else { 0 };
            let Some(text) = self.block(b, num) else { continue };
            if let Some(p) = prev {
                out.push_str(if is_list(p) && is_list(kind) { "\n" } else { "\n\n" });
            }
            out.push_str(&text);
            prev = Some(kind);
        }
        out
    }

    fn block(&mut self, b: &Block, num: usize) -> Option<String> {
        let d = b.data();
        let kind = b.kind();
        let text = match kind {
            "paragraph" => {
                let t = rich(&d["rich_text"]);
                let c = self.blocks(&b.children);
                if t.is_empty() && c.is_empty() {
                    return None;
                }
                join_blocks(t, indent(&c, "  "))
            }
            "heading_1" | "heading_2" | "heading_3" => {
                let level = kind.as_bytes()[8] - b'0';
                let h = format!("{} {}", "#".repeat(level as usize), rich(&d["rich_text"]));
                join_blocks(h, self.blocks(&b.children))
            }
            "bulleted_list_item" => self.list_item(b, "- ".to_string()),
            "numbered_list_item" => self.list_item(b, format!("{num}. ")),
            "to_do" => {
                let mark = if d["checked"].as_bool() == Some(true) { "- [x] " } else { "- [ ] " };
                self.list_item(b, mark.to_string())
            }
            "quote" => {
                let inner = join_blocks(rich(&d["rich_text"]), self.blocks(&b.children));
                quote(&inner)
            }
            "code" => {
                let lang = match d["language"].as_str().unwrap_or("") {
                    "plain text" => "txt",
                    other => other,
                };
                let body = plain(&d["rich_text"]);
                let fence = if body.contains("```") { "~~~~" } else { "```" };
                format!("{fence}{lang}\n{body}\n{fence}")
            }
            "callout" => {
                let icon = d["icon"]["emoji"].as_str().unwrap_or("");
                let color = d["color"].as_str().unwrap_or("default");
                let head = format!("{icon} [{color}]").trim_start().to_string();
                let body = join_blocks(rich(&d["rich_text"]), self.blocks(&b.children));
                let inner = if body.is_empty() { head } else { format!("{head}\n{body}") };
                quote(&inner)
            }
            "toggle" => format!(
                "<details>\n<summary>{}</summary>\n\n{}\n\n</details>",
                rich(&d["rich_text"]),
                self.blocks(&b.children)
            ),
            "table" => self.table(b),
            "divider" => "---".to_string(),
            "table_of_contents" => "[TOC]".to_string(),
            "child_page" => format!("📄 子页面：{}", d["title"].as_str().unwrap_or("")),
            "equation" => format!("$$\n{}\n$$", d["expression"].as_str().unwrap_or("")),
            "link_preview" | "bookmark" | "embed" => {
                let url = d["url"].as_str().unwrap_or("");
                let caption = rich(&d["caption"]);
                if caption.is_empty() { format!("<{url}>") } else { format!("[{caption}]({url})") }
            }
            _ => {
                self.warnings.push(format!("unsupported block: {kind} ({})", b.id()));
                format!("<!-- unsupported block: {kind} -->")
            }
        };
        Some(text)
    }

    fn list_item(&mut self, b: &Block, marker: String) -> String {
        let mut s = format!("{marker}{}", rich(&b.data()["rich_text"]));
        let children = self.blocks(&b.children);
        if !children.is_empty() {
            s.push('\n');
            s.push_str(&indent(&children, &" ".repeat(marker.chars().count())));
        }
        s
    }

    fn table(&mut self, b: &Block) -> String {
        let rows: Vec<Vec<String>> = b
            .children
            .iter()
            .filter(|r| r.kind() == "table_row")
            .map(|r| {
                r.data()["cells"]
                    .as_array()
                    .map(|cells| cells.iter().map(|c| table_cell(&rich(c))).collect())
                    .unwrap_or_default()
            })
            .collect();
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 {
            return String::new();
        }
        let line = |cells: &[String]| {
            let mut padded: Vec<&str> = cells.iter().map(String::as_str).collect();
            padded.resize(width, "");
            format!("| {} |", padded.join(" | "))
        };
        let mut out = vec![line(&rows[0]), format!("|{}", " --- |".repeat(width))];
        out.extend(rows[1..].iter().map(|r| line(r)));
        out.join("\n")
    }
}

/// Rich text -> inline Markdown. Adjacent bold runs are merged so Notion's
/// split runs do not produce `**a****b**`.
pub fn rich(v: &Value) -> String {
    let mut out = String::new();
    let mut bold = false;
    let mut just_opened = false;
    for run in v.as_array().into_iter().flatten() {
        let a = &run["annotations"];
        let is_bold = a["bold"].as_bool() == Some(true);
        if is_bold != bold {
            if is_bold {
                out.push_str("**");
                just_opened = true;
            } else {
                close_bold(&mut out);
            }
            bold = is_bold;
        }
        let mut text = run["plain_text"].as_str().unwrap_or("").to_string();
        if text.is_empty() {
            continue;
        }
        if just_opened {
            // CommonMark: "** x" is not bold; move leading spaces before "**".
            let trimmed = text.trim_start().to_string();
            let lead = text.len() - trimmed.len();
            if lead > 0 {
                out.truncate(out.len() - 2);
                out.push_str(&text[..lead]);
                out.push_str("**");
                text = trimmed;
            }
            just_opened = false;
        }
        if a["code"].as_bool() == Some(true) {
            text = if text.contains('`') { format!("`` {text} ``") } else { format!("`{text}`") };
        }
        if a["italic"].as_bool() == Some(true) {
            text = format!("*{text}*");
        }
        if a["strikethrough"].as_bool() == Some(true) {
            text = format!("~~{text}~~");
        }
        if run["type"] != "mention"
            && let Some(href) = run["href"].as_str()
        {
            text = format!("[{text}]({href})");
        }
        out.push_str(&text);
    }
    if bold {
        close_bold(&mut out);
    }
    out
}

/// Closes bold, keeping trailing whitespace outside the marker.
fn close_bold(out: &mut String) {
    let keep = out.trim_end().len();
    let ws = out.split_off(keep);
    out.push_str("**");
    out.push_str(&ws);
}

pub fn plain(v: &Value) -> String {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["plain_text"].as_str())
        .collect()
}

fn table_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', "<br>")
}

fn join_blocks(head: String, tail: String) -> String {
    match (head.is_empty(), tail.is_empty()) {
        (_, true) => head,
        (true, false) => tail,
        (false, false) => format!("{head}\n\n{tail}"),
    }
}

fn indent(s: &str, pad: &str) -> String {
    s.lines()
        .map(|l| if l.is_empty() { String::new() } else { format!("{pad}{l}") })
        .collect::<Vec<_>>()
        .join("\n")
}

fn quote(s: &str) -> String {
    s.lines()
        .map(|l| if l.is_empty() { ">".to_string() } else { format!("> {l}") })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t(s: &str) -> Value {
        json!({ "type": "text", "plain_text": s, "annotations": {}, "href": null })
    }
    fn bold(s: &str) -> Value {
        json!({ "type": "text", "plain_text": s, "annotations": { "bold": true }, "href": null })
    }
    fn code(s: &str) -> Value {
        json!({ "type": "text", "plain_text": s, "annotations": { "code": true }, "href": null })
    }
    fn block(kind: &str, data: Value, children: Vec<Value>) -> Value {
        json!({ "id": "b", "type": kind, kind: data, "children": children })
    }
    fn para(runs: Vec<Value>) -> Value {
        block("paragraph", json!({ "rich_text": runs }), vec![])
    }
    fn md(blocks: Vec<Value>) -> Rendered {
        let blocks: Vec<Block> = blocks.into_iter().map(Block::from_json).collect();
        render(&blocks)
    }

    #[test]
    fn merges_adjacent_bold_runs_and_keeps_spaces_outside() {
        let runs = json!([bold("为什么用 "), json!({
            "type": "text", "plain_text": "codex exec",
            "annotations": { "bold": true, "code": true }, "href": null
        }), t(" 而不是")]);
        assert_eq!(rich(&runs), "**为什么用 `codex exec`** 而不是");
    }

    #[test]
    fn links_but_not_mentions() {
        let runs = json!([
            { "type": "text", "plain_text": "EGM", "annotations": {}, "href": "https://x.test" },
            { "type": "mention", "plain_text": "产品文档模板与规范", "annotations": {}, "href": "https://notion.test/p" }
        ]);
        assert_eq!(rich(&runs), "[EGM](https://x.test)产品文档模板与规范");
    }

    #[test]
    fn nested_and_numbered_lists() {
        let r = md(vec![
            block("bulleted_list_item", json!({ "rich_text": [t("a")] }), vec![
                block("bulleted_list_item", json!({ "rich_text": [t("a1")] }), vec![]),
            ]),
            block("bulleted_list_item", json!({ "rich_text": [t("b")] }), vec![]),
            para(vec![t("p")]),
            block("numbered_list_item", json!({ "rich_text": [t("one")] }), vec![]),
            block("numbered_list_item", json!({ "rich_text": [t("two")] }), vec![]),
        ]);
        assert_eq!(r.markdown, "- a\n  - a1\n- b\n\np\n\n1. one\n2. two\n");
    }

    #[test]
    fn table_with_header_and_escaped_pipes() {
        let row = |cells: Vec<Vec<Value>>| block("table_row", json!({ "cells": cells }), vec![]);
        let r = md(vec![block(
            "table",
            json!({ "table_width": 2, "has_column_header": true }),
            vec![row(vec![vec![t("字段")], vec![t("说明")]]), row(vec![vec![code("a|b")], vec![t("x\ny")]])],
        )]);
        assert_eq!(r.markdown, "| 字段 | 说明 |\n| --- | --- |\n| `a\\|b` | x<br>y |\n");
    }

    #[test]
    fn callout_keeps_emoji_colour_and_children() {
        let r = md(vec![block(
            "callout",
            json!({ "rich_text": [bold("目标")], "icon": { "type": "emoji", "emoji": "✅" }, "color": "green_bg" }),
            vec![block("bulleted_list_item", json!({ "rich_text": [t("一")] }), vec![])],
        )]);
        assert_eq!(r.markdown, "> ✅ [green_bg]\n> **目标**\n>\n> - 一\n");
    }

    #[test]
    fn toggle_code_and_mermaid() {
        let r = md(vec![block(
            "toggle",
            json!({ "rich_text": [t("示例")] }),
            vec![block("code", json!({ "rich_text": [t("flowchart TD\n  A --> B")], "language": "mermaid" }), vec![])],
        )]);
        assert_eq!(
            r.markdown,
            "<details>\n<summary>示例</summary>\n\n```mermaid\nflowchart TD\n  A --> B\n```\n\n</details>\n"
        );
    }

    #[test]
    fn unsupported_block_leaves_placeholder_and_warning() {
        let r = md(vec![block("image", json!({}), vec![]), para(vec![t("after")])]);
        assert_eq!(r.markdown, "<!-- unsupported block: image -->\n\nafter\n");
        assert_eq!(r.warnings, vec!["unsupported block: image (b)".to_string()]);
    }

    #[test]
    fn link_blocks_become_links() {
        let r = md(vec![
            block("link_preview", json!({ "url": "https://a.test" }), vec![]),
            block("bookmark", json!({ "url": "https://b.test", "caption": [t("B")] }), vec![]),
        ]);
        assert_eq!(r.markdown, "<https://a.test>\n\n[B](https://b.test)\n");
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn empty_paragraphs_are_skipped() {
        let r = md(vec![para(vec![t("a")]), para(vec![]), para(vec![t("b")])]);
        assert_eq!(r.markdown, "a\n\nb\n");
    }
}
