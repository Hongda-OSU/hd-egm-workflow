//! PRD-specific block handling: splitting off the key-decision list (PRD §7.5).

use crate::markdown::plain;
use crate::notion::Block;
use crate::Result;

pub const KEY_DECISIONS_HEADING: &str = "关键决策清单";

fn heading_level(b: &Block) -> Option<u8> {
    match b.kind() {
        "heading_1" => Some(1),
        "heading_2" => Some(2),
        "heading_3" => Some(3),
        _ => None,
    }
}

/// True for "11. 关键决策清单" or "关键决策清单", but not for headings that
/// only mention it, such as "7.5 关键决策清单（写在 PRD 里）".
fn is_key_decisions_heading(b: &Block) -> bool {
    let text = plain(&b.data()["rich_text"]);
    let title = text.trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | '§' | ' '));
    heading_level(b).is_some() && title.trim() == KEY_DECISIONS_HEADING
}

/// Splits top-level blocks into (body, key-decision section).
///
/// The section starts at the heading titled "关键决策清单" (numbering aside) and
/// runs to the next heading of the same or higher level, or the end. A
/// divider right before it is dropped; child pages inside the range stay in
/// the body. Fails when the section is missing, appears more than once or
/// has no list items, because a round must not start without exactly one
/// (PRD §6.3, §7.6).
pub fn split_key_decisions(blocks: Vec<Block>) -> Result<(Vec<Block>, Vec<Block>)> {
    if blocks.iter().filter(|b| is_key_decisions_heading(b)).count() > 1 {
        return Err("PRD has more than one 关键决策清单 heading; ask the elephant to keep one".into());
    }
    let start = blocks
        .iter()
        .position(is_key_decisions_heading)
        .ok_or("PRD has no 关键决策清单 section; ask the elephant to add it")?;
    let level = heading_level(&blocks[start]).unwrap_or(1);
    let end = blocks[start + 1..]
        .iter()
        .position(|b| heading_level(b).is_some_and(|l| l <= level))
        .map_or(blocks.len(), |i| start + 1 + i);

    let divider_before = start > 0 && blocks[start - 1].kind() == "divider";

    let mut body = Vec::new();
    let mut key = Vec::new();
    for (i, b) in blocks.into_iter().enumerate() {
        if divider_before && i == start - 1 {
            continue;
        }
        if (start..end).contains(&i) && b.kind() != "child_page" {
            key.push(b);
        } else {
            body.push(b);
        }
    }

    let has_items = key[1..].iter().any(|b| {
        matches!(b.kind(), "bulleted_list_item" | "numbered_list_item" | "to_do")
    });
    if !has_items {
        return Err("PRD's 关键决策清单 section is empty; ask the elephant to fill it".into());
    }
    Ok((body, key))
}

/// Number of key decisions in `key-decisions.md`: each top-level list item
/// is one (PRD §7.6), bulleted, numbered or to-do alike. Nested items and
/// continuation lines are indented by the Markdown converter, so they do
/// not count.
pub fn count_key_items(markdown: &str) -> usize {
    markdown
        .lines()
        .filter(|l| {
            let digits = l.chars().take_while(char::is_ascii_digit).count();
            l.starts_with("- ") || (digits > 0 && l[digits..].starts_with(". "))
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn b(kind: &str, text: &str) -> Value {
        json!({ "id": text, "type": kind, kind: { "rich_text": [{ "plain_text": text, "annotations": {} }] } })
    }
    fn blocks(v: Vec<Value>) -> Vec<Block> {
        v.into_iter().map(Block::from_json).collect()
    }
    fn ids(v: &[Block]) -> Vec<&str> {
        v.iter().map(Block::id).collect()
    }

    #[test]
    fn splits_section_drops_divider_and_keeps_child_page() {
        let (body, key) = split_key_decisions(blocks(vec![
            b("heading_2", "10. 里程碑"),
            b("paragraph", "m"),
            json!({ "id": "div", "type": "divider", "divider": {} }),
            b("heading_2", "11. 关键决策清单"),
            b("bulleted_list_item", "k1"),
            b("bulleted_list_item", "k2"),
            json!({ "id": "child", "type": "child_page", "child_page": { "title": "M0" } }),
        ]))
        .unwrap();
        assert_eq!(ids(&body), vec!["10. 里程碑", "m", "child"]);
        assert_eq!(ids(&key), vec!["11. 关键决策清单", "k1", "k2"]);
    }

    #[test]
    fn section_ends_at_next_same_level_heading() {
        let (body, key) = split_key_decisions(blocks(vec![
            b("heading_2", "关键决策清单"),
            b("bulleted_list_item", "k1"),
            b("heading_3", "sub"),
            b("heading_2", "附录"),
        ]))
        .unwrap();
        assert_eq!(ids(&key), vec!["关键决策清单", "k1", "sub"]);
        assert_eq!(ids(&body), vec!["附录"]);
    }

    #[test]
    fn ignores_headings_that_only_mention_the_section() {
        let (body, key) = split_key_decisions(blocks(vec![
            b("heading_3", "7.5 关键决策清单（写在 PRD 里）"),
            b("paragraph", "说明"),
            b("heading_2", "11. 关键决策清单"),
            b("bulleted_list_item", "k1"),
        ]))
        .unwrap();
        assert_eq!(ids(&body), vec!["7.5 关键决策清单（写在 PRD 里）", "说明"]);
        assert_eq!(ids(&key), vec!["11. 关键决策清单", "k1"]);
    }

    #[test]
    fn a_second_section_is_an_error() {
        let r = split_key_decisions(blocks(vec![
            b("heading_2", "关键决策清单"),
            b("bulleted_list_item", "k1"),
            b("heading_2", "12. 关键决策清单"),
            b("bulleted_list_item", "k2"),
        ]));
        assert!(r.is_err());
    }

    #[test]
    fn counts_top_level_items_of_any_list_kind() {
        let md = "## 关键决策清单\n\n- a\n  - nested\n1. b\n2. c\n   continued\n- [ ] d\n- [x] e\n\nnote";
        assert_eq!(count_key_items(md), 5);
    }

    #[test]
    fn missing_or_empty_section_is_an_error() {
        assert!(split_key_decisions(blocks(vec![b("paragraph", "x")])).is_err());
        assert!(split_key_decisions(blocks(vec![b("heading_2", "关键决策清单"), b("paragraph", "x")])).is_err());
    }
}
