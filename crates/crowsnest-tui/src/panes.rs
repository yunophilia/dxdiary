//! Pane rendering.
//!
//! Every renderer registers its interactive regions in the [`HitMap`] as it
//! draws. Rebuilding the map each frame is what keeps clicks correct after a
//! resize or a scroll — there is no second source of truth to drift.

use crowsnest_core::document::render_line;
use crowsnest_core::{Document, HitMap, HitTarget, PaneId, Theme};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;

/// Resolved theme colours for one frame.
struct Palette {
    fg: Color,
    dim: Color,
    accent: Color,
    directory: Color,
    selection_bg: Color,
    gutter: Color,
    border: Color,
    border_focused: Color,
    warning: Color,
    added: Color,
    removed: Color,
    added_bg: Color,
    removed_bg: Color,
    hunk_header: Color,
}

impl Palette {
    fn new(theme: &Theme, depth: crowsnest_core::ColorDepth) -> Self {
        Self {
            fg: theme.fg.to_color(depth),
            dim: theme.dim.to_color(depth),
            accent: theme.accent.to_color(depth),
            directory: theme.directory.to_color(depth),
            selection_bg: theme.selection_bg.to_color(depth),
            gutter: theme.gutter.to_color(depth),
            border: theme.border.to_color(depth),
            border_focused: theme.border_focused.to_color(depth),
            warning: theme.warning.to_color(depth),
            added: theme.added.to_color(depth),
            removed: theme.removed.to_color(depth),
            added_bg: theme.added_bg.to_color(depth),
            removed_bg: theme.removed_bg.to_color(depth),
            hunk_header: theme.hunk_header.to_color(depth),
        }
    }
}

/// Render hunks as a unified diff.
///
/// Both line-number columns are shown, so a removal and the addition replacing
/// it can be traced to their real positions on each side — the gutter is the
/// part of a diff that answers "where in the file am I?".
fn diff_lines<'a>(
    diff: &'a crowsnest_vcs::FileDiff,
    app: &App,
    p: &Palette,
    width: usize,
    height: usize,
) -> Vec<Line<'a>> {
    use crowsnest_vcs::LineKind;

    if diff.binary {
        return vec![Line::from(Span::styled(
            "  Binary file — no line diff.",
            Style::default().fg(p.warning),
        ))];
    }

    // Width of each number column, from the largest number in the file, so it
    // never reflows mid-scroll.
    let widest = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter_map(|l| l.old_no.max(l.new_no))
        .max()
        .unwrap_or(1);
    let numw = widest.to_string().len().max(2);

    let mut out = Vec::new();
    let mut row = 0usize;
    let skip = app.doc_scroll_y;
    // Take the height from the layout, not from `app.content_h` — that is only
    // populated *after* a frame, so the first render would emit one line.
    let take = height.max(1);

    for hunk in &diff.hunks {
        if row >= skip && out.len() < take {
            out.push(Line::from(Span::styled(
                hunk.header(),
                Style::default()
                    .fg(p.hunk_header)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        row += 1;

        for line in &hunk.lines {
            if out.len() >= take {
                return out;
            }
            if row < skip {
                row += 1;
                continue;
            }

            let (fg, bg) = match line.kind {
                LineKind::Added => (p.added, Some(p.added_bg)),
                LineKind::Removed => (p.removed, Some(p.removed_bg)),
                LineKind::Context => (p.fg, None),
            };

            let num = |n: Option<u32>| match n {
                Some(v) => format!("{v:>numw$} "),
                None => " ".repeat(numw + 1),
            };

            let body = crowsnest_core::document::render_line(&line.text, app.config.tab_width);
            let visible: String = body
                .chars()
                .skip(app.doc_scroll_x)
                .take(width.saturating_sub(numw * 2 + 3))
                .collect();

            let mut style = Style::default().fg(fg);
            if let Some(bg) = bg {
                style = style.bg(bg);
            }

            let spans = vec![
                Span::styled(num(line.old_no), Style::default().fg(p.gutter)),
                Span::styled(num(line.new_no), Style::default().fg(p.gutter)),
                Span::styled(format!("{} ", line.kind.sigil()), style),
                Span::styled(visible, style),
            ];

            let rendered = Line::from(spans);
            out.push(if row == app.doc_line && app.focus == PaneId::Content {
                rendered.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                rendered
            });
            row += 1;
        }
    }
    out
}

fn frame(app: &App, pane: PaneId, title: &str, p: &Palette) -> Block<'static> {
    let focused = app.focus == pane;
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused { p.border_focused } else { p.border }))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(if focused { p.accent } else { p.dim })
                .add_modifier(if focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ))
}

pub(crate) fn render_tree(f: &mut Frame, area: Rect, app: &App, hits: &mut HitMap) {
    let p = Palette::new(&app.config.theme, app.depth);

    let title = app
        .tree
        .root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("root");
    let block = frame(app, PaneId::Tree, title, &p);
    let inner = block.inner(area);
    f.render_widget(block, area);

    hits.push(inner, HitTarget::TreeBody);
    // The title row focuses the pane without changing the selection.
    hits.push(
        Rect::new(area.x, area.y, area.width, 1),
        HitTarget::Header(PaneId::Tree),
    );

    let height = inner.height as usize;
    let rows = app.tree.rows();
    let visible = app.visible_rows();

    let mut lines = Vec::with_capacity(height);
    for (pos, &row_index) in visible
        .iter()
        .enumerate()
        .skip(app.tree_scroll)
        .take(height)
    {
        let row = &rows[row_index];
        let selected = pos == app.tree_sel;
        let indent = "  ".repeat(row.depth);

        let marker = if row.is_dir {
            if row.expanded {
                "▾ "
            } else {
                "▸ "
            }
        } else {
            "  "
        };

        // Fixed-width badge column, so names stay aligned whether or not a row
        // has one.
        let badge = app.badge_for(row);
        let badge_span = Span::styled(
            badge
                .map(|c| format!("{c} "))
                .unwrap_or_else(|| "  ".into()),
            Style::default().fg(match badge {
                Some('?') => p.dim,
                Some('D') => p.warning,
                Some('·') => p.gutter,
                Some(_) => p.accent,
                None => p.dim,
            }),
        );

        let name_style = Style::default().fg(if row.is_dir { p.directory } else { p.fg });
        let mut spans = vec![
            badge_span,
            Span::styled(indent, Style::default()),
            Span::styled(marker, Style::default().fg(p.dim)),
            Span::styled(row.name.clone(), name_style),
        ];

        if row.is_dir && row.expanded && app.tree.is_unreadable(&row.path) {
            spans.push(Span::styled(
                "  (unreadable)",
                Style::default().fg(p.warning),
            ));
        }

        let mut line = Line::from(spans);
        if selected {
            line = line.style(
                Style::default()
                    .bg(p.selection_bg)
                    .add_modifier(Modifier::BOLD),
            );
        }
        lines.push(line);
    }

    if visible.is_empty() {
        let msg = if app.changed_only {
            "  (no changed files)"
        } else {
            "  (empty)"
        };
        lines.push(Line::from(Span::styled(msg, Style::default().fg(p.dim))));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

pub(crate) fn render_content(f: &mut Frame, area: Rect, app: &App, hits: &mut HitMap) {
    let p = Palette::new(&app.config.theme, app.depth);

    let name = match &app.doc {
        Some(doc) => doc
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string(),
        None => "no file".to_string(),
    };
    // The title says which view you are in, because the two can look similar
    // at a glance on a file that is mostly context.
    let title = match (app.view, &app.diff) {
        (crate::app::ContentView::Diff, Some(d)) => {
            format!("{name}  diff · {}", d.summary())
        }
        (_, Some(_)) => format!("{name}  file · d for diff"),
        _ => name,
    };

    let block = frame(app, PaneId::Content, &title, &p);
    let inner = block.inner(area);
    f.render_widget(block, area);

    hits.push(inner, HitTarget::ContentBody);
    hits.push(
        Rect::new(area.x, area.y, area.width, 1),
        HitTarget::Header(PaneId::Content),
    );

    // Diff view short-circuits the document rendering below.
    if app.view == crate::app::ContentView::Diff {
        if let Some(d) = &app.diff {
            let lines = diff_lines(d, app, &p, inner.width as usize, inner.height as usize);
            f.render_widget(Paragraph::new(lines), inner);
            return;
        }
    }

    let lines: Vec<Line> = match &app.doc {
        None => vec![
            Line::from(Span::styled(
                "  Select a file in the tree.",
                Style::default().fg(p.dim),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  enter/click open · tab switch pane · r refresh · q quit",
                Style::default().fg(p.dim),
            )),
        ],
        Some(Document::Binary { bytes, .. }) => vec![Line::from(Span::styled(
            format!("  Binary file, {bytes} bytes."),
            Style::default().fg(p.warning),
        ))],
        Some(Document::TooLarge { bytes, .. }) => vec![Line::from(Span::styled(
            format!(
                "  {bytes} bytes exceeds max_file_bytes ({}).",
                app.config.max_file_bytes
            ),
            Style::default().fg(p.warning),
        ))],
        Some(Document::Error { message, .. }) => vec![Line::from(Span::styled(
            format!("  {message}"),
            Style::default().fg(p.warning),
        ))],
        Some(Document::Text(doc)) => {
            let height = inner.height as usize;
            // Gutter is sized to the real line count so it never reflows while
            // scrolling, which would make the text jitter sideways.
            let gutter = doc.line_count().max(1).to_string().len().max(3);
            let text_width = (inner.width as usize).saturating_sub(gutter + 1);

            doc.lines()
                .iter()
                .enumerate()
                .skip(app.doc_scroll_y)
                .take(height)
                .map(|(n, raw)| {
                    let expanded = render_line(raw, app.config.tab_width);
                    // Horizontal scrolling is by character, not byte: slicing a
                    // UTF-8 string by byte offset would panic mid-codepoint.
                    let visible: String = expanded
                        .chars()
                        .skip(app.doc_scroll_x)
                        .take(text_width)
                        .collect();

                    let current = n == app.doc_line;
                    let number = Span::styled(
                        format!("{:>width$} ", n + 1, width = gutter),
                        Style::default().fg(if current { p.accent } else { p.gutter }),
                    );
                    let body = Span::styled(visible, Style::default().fg(p.fg));

                    let line = Line::from(vec![number, body]);
                    if current && app.focus == PaneId::Content {
                        line.style(Style::default().bg(p.selection_bg))
                    } else {
                        line
                    }
                })
                .collect()
        }
    };

    f.render_widget(Paragraph::new(lines), inner);
}

pub(crate) fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let p = Palette::new(&app.config.theme, app.depth);

    let mut spans = Vec::new();

    // Branch and working-tree counts first — the state you glance at.
    if let Some(info) = &app.repo {
        spans.push(Span::styled(
            format!(" {} ", info.branch.as_deref().unwrap_or("detached")),
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
        ));

        let g = &app.git;
        if g.is_clean() {
            spans.push(Span::styled("clean ", Style::default().fg(p.dim)));
        } else {
            for (n, label, style) in [
                (g.staged.len(), "+", Style::default().fg(p.directory)),
                (g.unstaged.len(), "~", Style::default().fg(p.warning)),
                (g.untracked.len(), "?", Style::default().fg(p.dim)),
            ] {
                if n > 0 {
                    spans.push(Span::styled(format!("{label}{n} "), style));
                }
            }
        }

        spans.push(Span::styled(
            format!("│ {} ", app.baseline.label()),
            Style::default().fg(p.gutter),
        ));

        if app.changed_only {
            spans.push(Span::styled(
                "│ changed-only ",
                Style::default().fg(p.warning),
            ));
        }
    }

    spans.push(Span::styled(
        format!("│ {} ", app.status),
        Style::default().fg(p.dim),
    ));

    if matches!(&app.doc, Some(Document::Text(_))) {
        spans.push(Span::styled(
            format!(" {}:{} ", app.doc_line + 1, app.text_line_count().max(1)),
            Style::default().fg(p.gutter),
        ));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
