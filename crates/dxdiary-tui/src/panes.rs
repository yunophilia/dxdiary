//! Pane rendering.
//!
//! Every renderer registers its interactive regions in the [`HitMap`] as it
//! draws. Rebuilding the map each frame is what keeps clicks correct after a
//! resize or a scroll — there is no second source of truth to drift.

use dxdiary_core::document::render_line_mapped;
use dxdiary_core::{Document, HitMap, HitTarget, PaneId, Theme};
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
    syn_keyword: Color,
    syn_string: Color,
    syn_comment: Color,
    syn_function: Color,
    syn_type: Color,
    syn_number: Color,
    syn_constant: Color,
    syn_operator: Color,
    syn_punctuation: Color,
    syn_variable: Color,
    syn_attribute: Color,
}

impl Palette {
    fn new(theme: &Theme, depth: dxdiary_core::ColorDepth) -> Self {
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
            syn_keyword: theme.syn_keyword.to_color(depth),
            syn_string: theme.syn_string.to_color(depth),
            syn_comment: theme.syn_comment.to_color(depth),
            syn_function: theme.syn_function.to_color(depth),
            syn_type: theme.syn_type.to_color(depth),
            syn_number: theme.syn_number.to_color(depth),
            syn_constant: theme.syn_constant.to_color(depth),
            syn_operator: theme.syn_operator.to_color(depth),
            syn_punctuation: theme.syn_punctuation.to_color(depth),
            syn_variable: theme.syn_variable.to_color(depth),
            syn_attribute: theme.syn_attribute.to_color(depth),
        }
    }
}

/// Width of the blame gutter: `abcd1234 Author Name      3 days ago  `.
const BLAME_WIDTH: usize = 34;

/// One line's blame column.
///
/// Runs of lines from the same commit show the attribution only on the first,
/// the way GitLens and `tig blame` do — repeating it down a whole function is
/// noise that buries where authorship actually changes.
fn blame_span(app: &App, line: usize, p: &Palette) -> Span<'static> {
    let Some(blame) = &app.blame else {
        return Span::raw(" ".repeat(BLAME_WIDTH));
    };
    let Some(entry) = blame.get(line) else {
        return Span::raw(" ".repeat(BLAME_WIDTH));
    };

    let same_as_previous = line
        .checked_sub(1)
        .and_then(|prev| blame.get(prev))
        .is_some_and(|prev| prev.commit == entry.commit);

    if same_as_previous {
        return Span::raw(" ".repeat(BLAME_WIDTH));
    }

    // Truncate by characters: author names are not ASCII-only.
    let author: String = entry.author.chars().take(16).collect();
    let text = format!(
        "{:<8} {:<16} {:>7} ",
        entry.commit,
        author,
        entry.when.replace(" ago", "")
    );
    let text: String = text.chars().take(BLAME_WIDTH).collect();

    Span::styled(text, Style::default().fg(p.gutter))
}

/// Colour for one syntax role.
fn role_color(role: dxdiary_syntax::Role, p: &Palette) -> Color {
    use dxdiary_syntax::Role;
    match role {
        Role::Keyword => p.syn_keyword,
        Role::String => p.syn_string,
        Role::Comment => p.syn_comment,
        Role::Function => p.syn_function,
        Role::Type => p.syn_type,
        Role::Number => p.syn_number,
        Role::Constant => p.syn_constant,
        Role::Operator => p.syn_operator,
        Role::Punctuation => p.syn_punctuation,
        Role::Variable => p.syn_variable,
        Role::Attribute => p.syn_attribute,
        Role::Plain => p.fg,
    }
}

/// Split one rendered line into spans coloured by its syntax highlights.
///
/// `spans` are in character offsets against the *untruncated* line, so the
/// horizontal scroll window is applied here rather than by the caller — doing
/// it beforehand would leave the offsets pointing at the wrong characters.
fn highlighted_spans(
    text: &str,
    spans: &[dxdiary_syntax::Span],
    // Raw-character index to display column, from `render_line_mapped`.
    offsets: &[usize],
    scroll_x: usize,
    width: usize,
    base: Style,
    p: &Palette,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let end = chars.len().min(scroll_x.saturating_add(width));
    if scroll_x >= chars.len() {
        return vec![Span::styled(String::new(), base)];
    }

    // Per-character colour, then run-length encoded: simpler than interval
    // arithmetic, and highlight spans can overlap when captures nest.
    let mut colors: Vec<Option<Color>> = vec![None; chars.len()];
    // Spans index the raw line; `colors` indexes the expanded one.
    let column = |raw: usize| offsets.get(raw).copied().unwrap_or(chars.len());
    for s in spans {
        let color = role_color(s.role, p);
        for slot in colors
            .iter_mut()
            .take(column(s.end).min(chars.len()))
            .skip(column(s.start).min(chars.len()))
        {
            *slot = Some(color);
        }
    }

    let mut out: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_color: Option<Color> = colors.get(scroll_x).copied().flatten();

    for i in scroll_x..end {
        let c = colors[i];
        if c != run_color && !run.is_empty() {
            out.push(Span::styled(
                std::mem::take(&mut run),
                run_color.map_or(base, |col| base.fg(col)),
            ));
            run_color = c;
        } else if run.is_empty() {
            run_color = c;
        }
        run.push(chars[i]);
    }
    if !run.is_empty() {
        out.push(Span::styled(
            run,
            run_color.map_or(base, |col| base.fg(col)),
        ));
    }
    out
}

/// Render hunks as a unified diff.
///
/// Both line-number columns are shown, so a removal and the addition replacing
/// it can be traced to their real positions on each side — the gutter is the
/// part of a diff that answers "where in the file am I?".
fn diff_lines<'a>(
    diff: &'a dxdiary_vcs::FileDiff,
    app: &App,
    p: &Palette,
    width: usize,
    height: usize,
) -> Vec<Line<'a>> {
    use dxdiary_vcs::LineKind;

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

            let (body, offsets) = render_line_mapped(&line.text, app.config.tab_width);

            let mut style = Style::default().fg(fg);
            if let Some(bg) = bg {
                style = style.bg(bg);
            }

            // Look highlights up on whichever side this line belongs to, by
            // that side's own line number.
            let syntax = match (line.old_no, line.new_no) {
                (_, Some(n)) => app.new_spans.get(n as usize - 1),
                (Some(n), _) => app.old_spans.get(n as usize - 1),
                _ => None,
            };

            let text_width = width.saturating_sub(numw * 2 + 3);
            let mut spans = vec![
                Span::styled(num(line.old_no), Style::default().fg(p.gutter)),
                Span::styled(num(line.new_no), Style::default().fg(p.gutter)),
                Span::styled(format!("{} ", line.kind.sigil()), style),
            ];

            match syntax {
                // Added and removed lines keep their diff colour: the change is
                // what the eye needs first, and syntax colour would bury it.
                Some(s) if line.kind == LineKind::Context => spans.extend(highlighted_spans(
                    &body,
                    s,
                    &offsets,
                    app.doc_scroll_x,
                    text_width,
                    style,
                    p,
                )),
                _ => spans.push(Span::styled(
                    body.chars()
                        .skip(app.doc_scroll_x)
                        .take(text_width)
                        .collect::<String>(),
                    style,
                )),
            }

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
    let dirty = if app.is_dirty() { " ●" } else { "" };
    let mode = match app.mode {
        crate::app::Mode::Insert => "  INSERT",
        crate::app::Mode::Normal => "",
    };
    let title = match (app.view, &app.diff) {
        (crate::app::ContentView::Diff, Some(d)) => {
            format!("{name}{dirty}  diff · {}", d.summary())
        }
        (_, Some(_)) => format!("{name}{dirty}{mode}  file · d for diff"),
        _ => format!("{name}{dirty}{mode}"),
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
            // The buffer is the source of truth once a file is open; `doc` is
            // only a fallback for the moment before it exists.
            let owned = app.view_lines();
            let lines: &[String] = owned.as_deref().unwrap_or_else(|| doc.lines());
            // Gutter is sized to the real line count so it never reflows while
            // scrolling, which would make the text jitter sideways.
            let gutter = lines.len().max(1).to_string().len().max(3);

            // Blame column, when on and delivered. Fixed width for the same
            // reason as the line numbers.
            let blame_w = if app.show_blame && app.blame.is_some() {
                BLAME_WIDTH
            } else {
                0
            };
            // +1 for the line number's trailing space, +1 for the severity column.
            let text_width = (inner.width as usize).saturating_sub(gutter + 2 + blame_w);

            lines
                .iter()
                .enumerate()
                .skip(app.doc_scroll_y)
                .take(height)
                .map(|(n, raw)| {
                    let (expanded, offsets) = render_line_mapped(raw, app.config.tab_width);

                    let current = n == app.doc_line;
                    let number = Span::styled(
                        format!("{:>width$} ", n + 1, width = gutter),
                        Style::default().fg(if current { p.accent } else { p.gutter }),
                    );

                    // One column for LSP severity, always reserved so text does
                    // not shift as diagnostics arrive and clear.
                    let (mark, mark_color) = match app.diagnostic_at(n) {
                        Some(lsp_types::DiagnosticSeverity::ERROR) => ("✗", p.removed),
                        Some(lsp_types::DiagnosticSeverity::WARNING) => ("!", p.warning),
                        Some(_) => ("i", p.accent),
                        None => (" ", p.gutter),
                    };
                    let severity = Span::styled(mark.to_string(), Style::default().fg(mark_color));

                    // Horizontal scrolling is by character, not byte: slicing a
                    // UTF-8 string by byte offset would panic mid-codepoint.
                    let base = Style::default().fg(p.fg);
                    let mut spans = Vec::new();
                    if blame_w > 0 {
                        spans.push(blame_span(app, n, &p));
                    }
                    spans.push(severity);
                    spans.push(number);
                    match app.doc_spans.get(n) {
                        Some(s) => spans.extend(highlighted_spans(
                            &expanded,
                            s,
                            &offsets,
                            app.doc_scroll_x,
                            text_width,
                            base,
                            &p,
                        )),
                        None => spans.push(Span::styled(
                            expanded
                                .chars()
                                .skip(app.doc_scroll_x)
                                .take(text_width)
                                .collect::<String>(),
                            base,
                        )),
                    }

                    let line = Line::from(spans);
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
