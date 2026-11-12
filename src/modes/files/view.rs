//! Drawing of the Files toolbar and the file list.

use std::path::{Path, PathBuf};

use chrono::Local;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{ops, ops::Clip, tab::FilesTab};
use crate::ui::{bars, hit::HitMap, theme::Theme};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesTarget {
    Up,
    Refresh,
    Crumb(PathBuf),
    Row(usize),
    /// The scrollbar column; click or drag to scroll.
    Scrollbar,
}

const SIZE_W: u16 = 9;
const MODIFIED_W: u16 = 12;
const PAD: u16 = 2;
const GAP: u16 = 2;

pub fn fit(s: &str, width: usize) -> String {
    if s.width() <= width {
        return format!("{s}{}", " ".repeat(width - s.width()));
    }
    let mut out = String::new();
    for ch in s.chars() {
        if out.width() + ch.width().unwrap_or(0) + 1 > width {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    let w = out.width();
    format!("{out}{}", " ".repeat(width.saturating_sub(w)))
}

/// `root / home / deploy` with one hit target per part.
fn crumbs(path: &Path) -> Vec<(String, PathBuf)> {
    let mut out = vec![("root".to_string(), PathBuf::from("/"))];
    let mut acc = PathBuf::from("/");
    for part in path.iter().skip(1) {
        acc.push(part);
        out.push((part.to_string_lossy().into_owned(), acc.clone()));
    }
    out
}

pub fn render_toolbar(f: &mut Frame, area: Rect, theme: &Theme, tab: &FilesTab, hits: &mut HitMap<FilesTarget>) {
    bars::fill(f, area, Style::new().bg(theme.bg));
    let y = bars::mid(area);
    // Bottom border under the toolbar when there is room for it.
    if area.height >= 3 {
        let line = "─".repeat(area.width as usize);
        f.render_widget(
            Paragraph::new(Span::styled(line, Style::new().fg(theme.hover_bg))),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }
    let ctl_h = if area.height >= 3 { area.height - 1 } else { area.height };
    let at_root = tab.parent().is_none();
    let up = Rect::new(area.x + 1, area.y, 5, ctl_h);
    let up_style = Style::new().fg(if at_root { theme.fg_disabled } else { theme.fg }).bg(theme.raised_bg);
    bars::fill(f, up, up_style);
    f.render_widget(Paragraph::new(Span::styled("  ↑  ", up_style)), Rect::new(up.x, y.min(up.bottom() - 1), 5, 1));
    let y = y.min(up.bottom() - 1);
    if !at_root {
        hits.push(up, FilesTarget::Up);
    }

    let count = format!("{} {}", tab.entries.len(), if tab.entries.len() == 1 { "item" } else { "items" });
    let count_w = count.width() as u16;
    let right_limit = area.right().saturating_sub(count_w + 2);
    // Refresh button, right of "up".
    let refresh = Rect::new(up.right() + 1, area.y, 5, ctl_h);
    let refresh_style = Style::new().fg(if tab.loading { theme.accent } else { theme.fg }).bg(theme.raised_bg);
    bars::fill(f, refresh, refresh_style);
    f.render_widget(Paragraph::new(Span::styled("  ↻  ", refresh_style)), Rect::new(refresh.x, y, 5, 1));
    hits.push(refresh, FilesTarget::Refresh);

    let mut x = refresh.right() + 1;
    let parts = crumbs(&tab.path);
    let last = parts.len() - 1;
    // When the path is too long, keep the tail visible.
    let total: u16 = parts.iter().map(|(l, _)| l.width() as u16 + 3).sum();
    let mut skip = 0;
    let mut excess = (x + total).saturating_sub(right_limit);
    while excess > 0 && skip < last {
        excess = excess.saturating_sub(parts[skip].0.width() as u16 + 3);
        skip += 1;
    }
    if skip > 0 {
        f.render_widget(Paragraph::new(Span::styled(" … /", Style::new().fg(theme.fg_muted))), Rect::new(x, y, 4, 1));
        x += 4;
    }
    for (i, (label, target)) in parts.iter().enumerate().skip(skip) {
        // One column of padding on each side makes each crumb a wider button.
        let label = format!(" {label} ");
        let w = label.width() as u16;
        if x + w > right_limit {
            break;
        }
        let style = if i == last {
            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.fg_muted)
        };
        f.render_widget(Paragraph::new(Span::styled(label.clone(), style)), Rect::new(x, y, w, 1));
        hits.push(Rect::new(x, area.y, w, ctl_h), FilesTarget::Crumb(target.clone()));
        x += w;
        if i != last {
            f.render_widget(Paragraph::new(Span::styled("/", Style::new().fg(theme.fg_disabled))), Rect::new(x, y, 1, 1));
            x += 1;
        }
    }
    let rect = Rect::new(area.right().saturating_sub(count_w + 2), y, count_w, 1);
    f.render_widget(Paragraph::new(Span::styled(count, Style::new().fg(theme.fg_muted))), rect);
}

/// Draws the header and the rows; returns the rect of the scrolling body.
pub fn render_list(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    tab: &mut FilesTab,
    clip: Option<&Clip>,
    focused: bool,
    hits: &mut HitMap<FilesTarget>,
) -> Rect {
    if area.height == 0 {
        return area;
    }
    // One column on the right for the scrollbar, only when the folder does not fit.
    let body_h = area.height.saturating_sub(1) as usize;
    let scrollable = tab.entries.len() > body_h && area.width > 20;
    let full = area;
    let area = if scrollable { Rect { width: area.width - 1, ..area } } else { area };
    let name_w = area.width.saturating_sub(PAD * 2 + SIZE_W + MODIFIED_W + GAP * 2) as usize;
    let header = Rect::new(area.x, area.y, area.width, 1);
    let line = format!(
        "{}{}{}{}{}{}",
        " ".repeat(PAD as usize),
        fit("Name", name_w),
        " ".repeat(GAP as usize),
        fit("Size", SIZE_W as usize),
        " ".repeat(GAP as usize),
        fit("Modified", MODIFIED_W as usize),
    );
    f.render_widget(
        Paragraph::new(Line::styled(line, Style::new().fg(theme.fg_muted).add_modifier(Modifier::UNDERLINED)))
            .style(Style::new().bg(theme.bg)),
        header,
    );

    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    tab.ensure_cursor_visible(body.height as usize);
    if scrollable {
        let bar = Rect::new(full.right() - 1, body.y, 1, body.height);
        render_scrollbar(f, bar, theme, tab.scroll, body.height as usize, tab.entries.len(), focused);
        hits.push(bar, FilesTarget::Scrollbar);
    }
    // The returned body keeps the scrollbar column, so wheel events over it scroll too.
    let list_body = Rect { width: full.width, ..body };

    let notice = if let Some(err) = &tab.error {
        Some(err.clone())
    } else if tab.entries.is_empty() {
        Some(if tab.loading { "Loading…".into() } else { "Empty folder".into() })
    } else {
        None
    };
    if let Some(text) = notice {
        let rect = Rect::new(body.x, body.y + body.height.min(2), body.width, 1);
        f.render_widget(
            Paragraph::new(Span::styled(text, Style::new().fg(theme.fg_muted))).alignment(ratatui::layout::Alignment::Center),
            rect,
        );
        return list_body;
    }

    let now = Local::now();
    for (row, idx) in (tab.scroll..tab.entries.len()).take(body.height as usize).enumerate() {
        let e = &tab.entries[idx];
        let rect = Rect::new(body.x, body.y + row as u16, body.width, 1);
        let selected = tab.selection.contains(&e.name);
        let cursor = focused && idx == tab.cursor;
        let marked = clip.is_some_and(|c| c.op == ops::ClipOp::Move && c.contains(&tab.path, &e.name));

        let bg = if selected { theme.accent_tint } else if cursor { theme.raised_bg } else { theme.bg };
        let fg = if marked { theme.fg_dimmed } else { theme.fg };
        let muted = if marked { theme.fg_dimmed } else { theme.fg_muted };
        let base = Style::new().bg(bg);

        let name = e.name.to_string_lossy();
        let (icon, icon_style) = if e.is_dir() {
            ("▸ ", base.fg(if marked { theme.fg_dimmed } else { theme.accent }))
        } else {
            ("· ", base.fg(muted))
        };
        let label = if e.is_dir() { format!("{name}/") } else { name.into_owned() };
        let mut name_style = base.fg(fg);
        if e.symlink {
            name_style = name_style.add_modifier(Modifier::ITALIC);
        }
        if selected {
            name_style = name_style.add_modifier(Modifier::BOLD);
        }
        let cursor_mark = if cursor { Span::styled("›", base.fg(theme.accent)) } else { Span::styled(" ", base) };
        let size = if e.is_dir() { String::new() } else { ops::human_size(e.size) };
        let spans = vec![
            cursor_mark,
            Span::styled(" ".repeat(PAD as usize - 1), base),
            Span::styled(icon, icon_style),
            Span::styled(fit(&label, name_w.saturating_sub(2)), name_style),
            Span::styled(" ".repeat(GAP as usize), base),
            Span::styled(format!("{size:>w$}", w = SIZE_W as usize), base.fg(muted)),
            Span::styled(" ".repeat(GAP as usize), base),
            Span::styled(fit(&ops::format_modified(e.modified, now), MODIFIED_W as usize), base.fg(muted)),
            Span::styled(" ".repeat(PAD as usize), base),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)).style(base), rect);
        hits.push(rect, FilesTarget::Row(idx));
    }
    list_body
}

/// Position and length of the thumb in a track of `height` rows: `(top, len)`.
pub fn thumb(scroll: usize, height: usize, total: usize) -> (usize, usize) {
    if total <= height || height == 0 {
        return (0, height);
    }
    let len = (height * height / total).clamp(1, height);
    let max_scroll = total - height;
    let top = (scroll.min(max_scroll) * (height - len) + max_scroll / 2) / max_scroll;
    (top, len)
}

/// The scroll offset that centers the list on row `row` of the track.
pub fn scroll_for_row(row: usize, height: usize, total: usize) -> usize {
    if total <= height || height == 0 {
        return 0;
    }
    let point = (row * total + total / 2) / height;
    point.saturating_sub(height / 2).min(total - height)
}

fn render_scrollbar(f: &mut Frame, bar: Rect, theme: &Theme, scroll: usize, height: usize, total: usize, focused: bool) {
    let (top, len) = thumb(scroll, height, total);
    let thumb_bg = if focused { theme.fg_disabled } else { theme.border };
    for row in 0..bar.height as usize {
        let on = row >= top && row < top + len;
        let style = Style::new().bg(if on { thumb_bg } else { theme.raised_bg });
        f.render_widget(Paragraph::new(" ").style(style), Rect::new(bar.x, bar.y + row as u16, 1, 1));
    }
}

#[cfg(test)]
mod scrollbar_tests {
    use super::*;

    #[test]
    fn thumb_covers_the_track_when_everything_fits() {
        assert_eq!(thumb(0, 10, 5), (0, 10));
        assert_eq!(thumb(0, 10, 10), (0, 10));
    }

    #[test]
    fn thumb_moves_from_top_to_bottom() {
        // 100 entries in 10 rows: a one-row thumb.
        assert_eq!(thumb(0, 10, 100), (0, 1));
        assert_eq!(thumb(90, 10, 100), (9, 1));
        let (top, len) = thumb(45, 10, 100);
        assert_eq!(len, 1);
        assert!((4..=5).contains(&top));
        // Scrolled past the end (stale value) still stays in the track.
        assert_eq!(thumb(500, 10, 100), (9, 1));
    }

    #[test]
    fn clicking_the_track_jumps_there() {
        assert_eq!(scroll_for_row(0, 10, 100), 0);
        assert_eq!(scroll_for_row(9, 10, 100), 90);
        assert_eq!(scroll_for_row(5, 10, 100), 50);
        assert_eq!(scroll_for_row(3, 10, 8), 0);
    }
}
