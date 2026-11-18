//! Snapshot diffs between two rounds (PRD §6.4, M5): how the sections an
//! issue names changed in the next round.

use serde::Serialize;
use similar::{ChangeTag, TextDiff};

/// Unchanged lines kept around each change; longer runs become a gap.
const CONTEXT: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Line {
    Same { text: String },
    Add { text: String },
    Del { text: String },
    /// This many unchanged lines were folded away.
    Gap { count: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionStatus {
    Changed,
    Unchanged,
    /// No heading with this text (or number) in one of the two snapshots.
    NotFound,
    /// More than one heading matches, so the section cannot be told apart.
    Ambiguous,
}

/// The diff of one section an issue names (`Issue.section`).
#[derive(Debug, Clone, Serialize)]
pub struct SectionDiff {
    pub reference: String,
    pub status: SectionStatus,
    pub added: usize,
    pub removed: usize,
    pub lines: Vec<Line>,
}

/// Level and text of a heading line, e.g. `### 7.2 Foo` -> (3, "7.2 Foo").
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|&c| c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &line[level..];
    rest.starts_with(' ').then(|| (level, rest.trim()))
}

fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

/// Every line paired with whether it is a heading outside a code fence.
fn headings(md: &str) -> Vec<(usize, Option<(usize, &str)>)> {
    let mut in_fence = false;
    md.lines()
        .enumerate()
        .map(|(i, l)| {
            if is_fence(l) {
                in_fence = !in_fence;
                return (i, None);
            }
            (i, if in_fence { None } else { heading(l) })
        })
        .collect()
}

/// Splits "7.2 Foo" / "§5. Foo" / "§6.1" into a number ("7.2") and a title.
fn split_number(text: &str) -> (&str, &str) {
    let text = text.trim().trim_start_matches('§').trim();
    let end = text.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(text.len());
    (text[..end].trim_end_matches('.'), text[end..].trim())
}

enum Found {
    Text(String),
    Missing,
    Ambiguous,
}

/// A heading line of a snapshot: line index, level, number and title.
struct Heading<'a> {
    line: usize,
    level: usize,
    num: &'a str,
    title: &'a str,
}

/// The block of `md` under the heading `reference` names: the heading and
/// everything up to the next heading of the same or a higher level (so it
/// includes sub-headings). Matched by title text, then by a title that
/// starts with it, then by section number.
fn find_section(md: &str, reference: &str) -> Found {
    let (num, title) = split_number(reference);
    let hs: Vec<Heading> = headings(md)
        .into_iter()
        .filter_map(|(line, h)| {
            h.map(|(level, text)| {
                let (num, title) = split_number(text);
                Heading { line, level, num, title }
            })
        })
        .collect();
    let tries: [&dyn Fn(&Heading) -> bool; 3] = [
        &|h| !title.is_empty() && h.title == title,
        &|h| !title.is_empty() && h.title.starts_with(title),
        &|h| !num.is_empty() && h.num == num,
    ];
    for f in tries {
        let hits: Vec<&Heading> = hs.iter().filter(|h| f(h)).collect();
        match hits.as_slice() {
            [] => continue,
            [h] => {
                let end = hs.iter().find(|x| x.line > h.line && x.level <= h.level).map_or(usize::MAX, |x| x.line);
                let text: Vec<&str> = md.lines().skip(h.line).take(end.saturating_sub(h.line)).collect();
                return Found::Text(text.join("\n"));
            }
            _ => return Found::Ambiguous,
        }
    }
    Found::Missing
}

/// Folds runs of unchanged lines, keeping CONTEXT lines around changes and
/// the first line (the section heading).
fn collapse(lines: Vec<Line>) -> Vec<Line> {
    let changed: Vec<bool> = lines.iter().map(|l| !matches!(l, Line::Same { .. })).collect();
    let keep = |i: usize| {
        i == 0
            || changed[i.saturating_sub(CONTEXT)..=(i + CONTEXT).min(lines.len() - 1)].iter().any(|&c| c)
    };
    let mut out = Vec::new();
    let mut gap = 0;
    for (i, l) in lines.iter().enumerate() {
        if keep(i) {
            if gap > 0 {
                out.push(Line::Gap { count: gap });
                gap = 0;
            }
            out.push(l.clone());
        } else {
            gap += 1;
        }
    }
    if gap > 0 {
        out.push(Line::Gap { count: gap });
    }
    out
}

fn count(lines: &[Line]) -> (usize, usize) {
    lines.iter().fold((0, 0), |(a, r), l| match l {
        Line::Add { .. } => (a + 1, r),
        Line::Del { .. } => (a, r + 1),
        _ => (a, r),
    })
}

fn line_diff(old: &str, new: &str) -> Vec<Line> {
    TextDiff::from_lines(old, new)
        .iter_all_changes()
        .map(|c| {
            let text = c.value().trim_end_matches(['\n', '\r']).to_string();
            match c.tag() {
                ChangeTag::Equal => Line::Same { text },
                ChangeTag::Insert => Line::Add { text },
                ChangeTag::Delete => Line::Del { text },
            }
        })
        .collect()
}

/// Diffs each section `field` names (an issue's `section`, parts separated
/// by `；` or `;`) between two snapshots.
pub fn issue_sections(field: &str, old: &str, new: &str) -> Vec<SectionDiff> {
    let mut seen = Vec::new();
    field
        .split(['；', ';'])
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .filter(|r| {
            let fresh = !seen.contains(r);
            seen.push(r);
            fresh
        })
        .map(|reference| {
            let (status, lines) = match (find_section(old, reference), find_section(new, reference)) {
                (Found::Ambiguous, _) | (_, Found::Ambiguous) => (SectionStatus::Ambiguous, vec![]),
                (Found::Text(a), Found::Text(b)) => {
                    let lines = line_diff(&a, &b);
                    if lines.iter().all(|l| matches!(l, Line::Same { .. })) {
                        (SectionStatus::Unchanged, vec![])
                    } else {
                        (SectionStatus::Changed, collapse(lines))
                    }
                }
                (Found::Missing, _) | (_, Found::Missing) => (SectionStatus::NotFound, vec![]),
            };
            let (added, removed) = count(&lines);
            SectionDiff { reference: reference.to_string(), status, added, removed, lines }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "# Title\nintro\n## 5. Data flow\na\nb\nc\nd\ne\nf\n### 5.1 Sync\ns1\n## 6. Features\nx\n```sh\n# not a heading\n```\n";

    #[test]
    fn long_unchanged_runs_fold_into_a_gap() {
        let new = OLD.replace("\nf\n", "\nf2\n");
        let d = &issue_sections("§5 Data flow", OLD, &new)[0];
        assert_eq!(d.lines[0], Line::Same { text: "## 5. Data flow".into() });
        assert_eq!(d.lines[1], Line::Gap { count: 3 });
        assert!(d.lines.contains(&Line::Add { text: "f2".into() }));
    }

    #[test]
    fn hash_lines_inside_code_fences_are_not_headings() {
        let d = issue_sections("not a heading", OLD, OLD);
        assert_eq!(d[0].status, SectionStatus::NotFound);
    }

    #[test]
    fn issue_section_matches_by_title_and_includes_subsections() {
        let new = OLD.replace("s1\n", "s1 changed\n");
        let d = issue_sections("§5 Data flow；§6. Features", OLD, &new);
        assert_eq!(d[0].status, SectionStatus::Changed);
        assert_eq!((d[0].added, d[0].removed), (1, 1));
        assert_eq!(d[1].status, SectionStatus::Unchanged);
    }

    #[test]
    fn issue_section_falls_back_to_number_and_prefix() {
        let d = issue_sections("§5.1；§6 Feat", OLD, OLD);
        assert_eq!(d[0].status, SectionStatus::Unchanged);
        assert_eq!(d[1].status, SectionStatus::Unchanged);
    }

    #[test]
    fn issue_section_reports_missing_and_duplicate_headings() {
        let dup = format!("{OLD}## 5. Data flow\nagain\n");
        let d = issue_sections("§9 Nowhere；§5 Data flow", OLD, &dup);
        assert_eq!(d[0].status, SectionStatus::NotFound);
        assert_eq!(d[1].status, SectionStatus::Ambiguous);
    }
}
