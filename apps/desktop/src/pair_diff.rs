//! Align two versions of a file into paired rows for a two-pane ("before |
//! after") diff.
//!
//! Distinct from [`crate::git`]'s diff module, which *parses* unified diff text
//! that `git` produced: here we have both full texts — the session's baseline from
//! the change ledger and what is on disk now — and compute the alignment ourselves.
//! A unified diff can't drive a two-pane view, because it never says which removed
//! line stands opposite which added one.
//!
//! Pure and GPUI-free so the alignment is unit-testable on its own.

use similar::{capture_diff_slices, Algorithm, DiffOp};

/// What happened on one row of the paired view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// Both sides carry the same line.
    Equal,
    /// Only the right side has a line — it was added.
    Insert,
    /// Only the left side has a line — it was removed.
    Delete,
    /// Both sides have a line and they differ.
    Replace,
}

impl RowKind {
    /// Whether this row is part of a change (everything but [`RowKind::Equal`]).
    pub fn is_change(self) -> bool {
        !matches!(self, RowKind::Equal)
    }
}

/// One line as shown in a pane, with the line number that pane's gutter displays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// 1-based line number within its own side.
    pub number: u32,
    pub text: String,
}

/// One row of the two-pane view. A side is `None` where that pane shows a filler
/// gap, which is what keeps the two panes scrolling in step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub left: Option<DiffLine>,
    pub right: Option<DiffLine>,
    pub kind: RowKind,
}

/// Align `before` and `after` into paired rows.
///
/// Within a replaced block, removed and added lines are paired positionally (first
/// with first, and so on) and the surplus on either side becomes single-sided rows
/// — the alignment a reviewer expects when a line is rewritten in place.
pub fn side_by_side(before: &str, after: &str) -> Vec<DiffRow> {
    let left: Vec<&str> = before.lines().collect();
    let right: Vec<&str> = after.lines().collect();
    let mut rows = Vec::new();

    for op in capture_diff_slices(Algorithm::Myers, &left, &right) {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for i in 0..len {
                    rows.push(DiffRow {
                        left: line(&left, old_index + i),
                        right: line(&right, new_index + i),
                        kind: RowKind::Equal,
                    });
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for i in 0..old_len {
                    rows.push(DiffRow {
                        left: line(&left, old_index + i),
                        right: None,
                        kind: RowKind::Delete,
                    });
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for i in 0..new_len {
                    rows.push(DiffRow {
                        left: None,
                        right: line(&right, new_index + i),
                        kind: RowKind::Insert,
                    });
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                for i in 0..old_len.max(new_len) {
                    let l = (i < old_len).then(|| line(&left, old_index + i)).flatten();
                    let r = (i < new_len).then(|| line(&right, new_index + i)).flatten();
                    let kind = match (&l, &r) {
                        (Some(_), Some(_)) => RowKind::Replace,
                        (Some(_), None) => RowKind::Delete,
                        _ => RowKind::Insert,
                    };
                    rows.push(DiffRow {
                        left: l,
                        right: r,
                        kind,
                    });
                }
            }
        }
    }
    rows
}

/// Contiguous runs of changed rows — what "jump to next change" steps through, and
/// what a collapsed view would keep while hiding the equal stretches between them.
pub fn change_blocks(rows: &[DiffRow]) -> Vec<std::ops::Range<usize>> {
    let mut blocks: Vec<std::ops::Range<usize>> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, row) in rows.iter().enumerate() {
        match (row.kind.is_change(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                blocks.push(s..i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        blocks.push(s..rows.len());
    }
    blocks
}

/// Whether the two sides differ at all — a file the session rewrote to the same
/// content is worth showing as "no net change" rather than an empty diff.
pub fn has_changes(rows: &[DiffRow]) -> bool {
    rows.iter().any(|row| row.kind.is_change())
}

/// Runs of unchanged rows worth collapsing, keeping `context` rows either side of
/// every change.
///
/// This is what makes a large file reviewable: a 3000-line source with two edits is
/// otherwise 3000 rows of identical text in both panes, which buries the change and
/// costs a full pane of layout work per frame. Runs no longer than `context * 2` are
/// left alone — hiding four lines behind a "4 lines hidden" marker saves nothing and
/// reads worse.
pub fn foldable_runs(rows: &[DiffRow], context: usize) -> Vec<std::ops::Range<usize>> {
    let changes = change_blocks(rows);
    if changes.is_empty() {
        // No changes at all: the whole file is one collapsible run. Bound to a
        // local first — `vec![0..n]` reads to clippy as a mistyped `vec![0; n]`.
        if rows.is_empty() {
            return Vec::new();
        }
        let whole_file = 0..rows.len();
        return vec![whole_file];
    }

    let mut runs = Vec::new();
    let mut cursor = 0usize;
    for block in &changes {
        // The unchanged stretch before this change, minus its trailing context.
        let stop = block.start.saturating_sub(context);
        push_run(&mut runs, cursor, stop, context);
        cursor = (block.end + context).min(rows.len());
    }
    // …and the tail after the last change.
    push_run(&mut runs, cursor, rows.len(), context);
    runs
}

/// Record `start..end` as foldable when it is long enough to be worth hiding.
fn push_run(runs: &mut Vec<std::ops::Range<usize>>, start: usize, end: usize, context: usize) {
    let worth_hiding = context.max(1) + 1;
    if end > start && end - start >= worth_hiding {
        runs.push(start..end);
    }
}

/// The `index`-th line of `lines`, numbered 1-based for its gutter.
fn line(lines: &[&str], index: usize) -> Option<DiffLine> {
    lines.get(index).map(|text| DiffLine {
        number: index as u32 + 1,
        text: (*text).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compact view of a row: (left text, right text, kind).
    fn shape(rows: &[DiffRow]) -> Vec<(Option<&str>, Option<&str>, RowKind)> {
        rows.iter()
            .map(|r| {
                (
                    r.left.as_ref().map(|l| l.text.as_str()),
                    r.right.as_ref().map(|l| l.text.as_str()),
                    r.kind,
                )
            })
            .collect()
    }

    #[test]
    fn identical_texts_are_all_equal_rows() {
        let rows = side_by_side("a\nb\n", "a\nb\n");
        assert_eq!(
            shape(&rows),
            vec![
                (Some("a"), Some("a"), RowKind::Equal),
                (Some("b"), Some("b"), RowKind::Equal),
            ]
        );
        assert!(!has_changes(&rows));
        assert!(change_blocks(&rows).is_empty());
    }

    #[test]
    fn an_added_line_pads_the_left_pane() {
        let rows = side_by_side("a\nc\n", "a\nb\nc\n");
        assert_eq!(
            shape(&rows),
            vec![
                (Some("a"), Some("a"), RowKind::Equal),
                (None, Some("b"), RowKind::Insert),
                (Some("c"), Some("c"), RowKind::Equal),
            ]
        );
        // Gutters number within their own side, so the right pane keeps counting
        // past the filler while the left does not.
        assert_eq!(rows[2].left.as_ref().unwrap().number, 2);
        assert_eq!(rows[2].right.as_ref().unwrap().number, 3);
    }

    #[test]
    fn a_removed_line_pads_the_right_pane() {
        let rows = side_by_side("a\nb\nc\n", "a\nc\n");
        assert_eq!(
            shape(&rows),
            vec![
                (Some("a"), Some("a"), RowKind::Equal),
                (Some("b"), None, RowKind::Delete),
                (Some("c"), Some("c"), RowKind::Equal),
            ]
        );
    }

    #[test]
    fn a_rewritten_line_pairs_opposite_its_replacement() {
        let rows = side_by_side("keep\nold\n", "keep\nnew\n");
        assert_eq!(
            shape(&rows),
            vec![
                (Some("keep"), Some("keep"), RowKind::Equal),
                (Some("old"), Some("new"), RowKind::Replace),
            ]
        );
    }

    #[test]
    fn an_uneven_replacement_pairs_then_spills() {
        // Two lines become three: the first two pair up, the surplus is an insert.
        let rows = side_by_side("one\ntwo\n", "1\n2\n3\n");
        assert_eq!(
            shape(&rows),
            vec![
                (Some("one"), Some("1"), RowKind::Replace),
                (Some("two"), Some("2"), RowKind::Replace),
                (None, Some("3"), RowKind::Insert),
            ]
        );
    }

    #[test]
    fn a_created_file_is_all_inserts() {
        // The baseline of a file the agent created is empty — every line is new.
        let rows = side_by_side("", "a\nb\n");
        assert_eq!(
            shape(&rows),
            vec![
                (None, Some("a"), RowKind::Insert),
                (None, Some("b"), RowKind::Insert),
            ]
        );
        assert!(has_changes(&rows));
    }

    #[test]
    fn both_empty_yields_no_rows() {
        assert!(side_by_side("", "").is_empty());
    }

    #[test]
    fn change_blocks_group_contiguous_runs() {
        // equal, change, change, equal, change  →  two blocks
        let rows = side_by_side("a\nb\nc\nd\ne\n", "a\nB\nC\nd\nE\n");
        assert_eq!(change_blocks(&rows), vec![1..3, 4..5]);
    }

    /// Build `n` identical lines either side, with one changed line at `at`.
    fn with_change_at(n: usize, at: usize) -> Vec<DiffRow> {
        let before: String = (0..n).map(|i| format!("line {i}\n")).collect();
        let after: String = (0..n)
            .map(|i| {
                if i == at {
                    "CHANGED\n".to_string()
                } else {
                    format!("line {i}\n")
                }
            })
            .collect();
        side_by_side(&before, &after)
    }

    #[test]
    fn folding_keeps_context_either_side_of_a_change() {
        // 20 rows, one change at row 10, 3 rows of context → hide 0..7 and 14..20.
        let rows = with_change_at(20, 10);
        assert_eq!(foldable_runs(&rows, 3), vec![0..7, 14..20]);
    }

    #[test]
    fn a_short_gap_between_changes_is_left_expanded() {
        // Two changes four rows apart with 3 context each: the gap is fully covered
        // by context, so there is nothing to hide between them.
        let before: String = (0..12).map(|i| format!("line {i}\n")).collect();
        let mut after: Vec<String> = (0..12).map(|i| format!("line {i}\n")).collect();
        after[5] = "A\n".into();
        after[8] = "B\n".into();
        let rows = side_by_side(&before, &after.concat());
        let runs = foldable_runs(&rows, 3);
        assert!(
            !runs.iter().any(|r| r.start >= 5 && r.end <= 9),
            "the gap between the two changes stays visible: {runs:?}"
        );
    }

    #[test]
    fn a_file_with_no_changes_folds_entirely() {
        let rows = side_by_side("a\nb\nc\nd\ne\n", "a\nb\nc\nd\ne\n");
        assert_eq!(foldable_runs(&rows, 3), vec![0..5]);
        assert!(foldable_runs(&[], 3).is_empty(), "nothing to fold");
    }

    #[test]
    fn a_change_at_the_very_start_folds_only_the_tail() {
        let rows = with_change_at(20, 0);
        assert_eq!(foldable_runs(&rows, 3), vec![4..20]);
    }

    #[test]
    fn every_folded_run_is_entirely_unchanged() {
        // The invariant that matters: folding must never hide a change.
        let rows = with_change_at(40, 20);
        for run in foldable_runs(&rows, 3) {
            assert!(
                rows[run.clone()].iter().all(|r| !r.kind.is_change()),
                "run {run:?} hides a change"
            );
        }
    }

    #[test]
    fn a_trailing_change_closes_its_block() {
        let rows = side_by_side("a\n", "a\nb\n");
        assert_eq!(change_blocks(&rows), vec![1..2], "the run reaches the end");
    }
}
