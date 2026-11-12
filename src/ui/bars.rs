//! Tab bar (top), terminal header (between the panes) and status line (bottom).
//!
//! The tab and bottom bars are 3 rows tall (1 in compact mode on small screens); text sits
//! on the middle row and every button's hit area covers the full height, like the
//! prototype's 44px controls. The terminal header is a single row.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use std::path::Path;

use super::{TermSize, hit::HitMap, theme::Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeTarget {
    Tab(usize),
    CloseTab(usize),
    NewTab,
    /// The `TERMINAL` title: opens a minimized terminal, minimizes an open one.
    TermTitle,
    TermSize(TermSize),
    /// A terminal tab of the current workspace.
    Term(usize),
    CloseTerm(usize),
    NewTerm,
    /// Opens the menu of leader-key shortcuts.
    Shortcuts,
    Quit,
}

#[derive(Debug, Clone)]
pub struct TabInfo {
    pub title: String,
    /// Commands running in the tab's terminals.
    pub running: usize,
}

/// `~/apps` for folders under `$HOME`.
pub fn tilde(path: &Path) -> String {
    if let Some(home) = dirs::home_dir().filter(|h| h.as_os_str().len() > 1)
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
    }
    path.display().to_string()
}

const MAX_TITLE: usize = 24;

pub fn truncate(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        if out.width() + 1 >= max {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// Like [`truncate`], but keeps the end: `…/scratchpad/play`.
pub fn truncate_start(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut w = 1;
    for ch in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max {
            break;
        }
        w += cw;
        tail.push(ch);
    }
    let mut out = String::from(if max > 0 { "…" } else { "" });
    out.extend(tail.into_iter().rev());
    out
}

pub fn fill(f: &mut Frame, rect: Rect, style: Style) {
    f.render_widget(Block::default().style(style), rect);
}

/// The row where text goes inside a control of `area`'s height.
pub fn mid(area: Rect) -> u16 {
    area.y + area.height / 2
}

fn text_at(f: &mut Frame, x: u16, y: u16, spans: Vec<Span<'static>>) -> u16 {
    let line = Line::from(spans);
    let w = line.width() as u16;
    f.render_widget(Paragraph::new(line), Rect::new(x, y, w, 1));
    w
}

fn vline(f: &mut Frame, x: u16, area: Rect, theme: &Theme) {
    let lines: Vec<Line> = (0..area.height).map(|_| Line::raw("│")).collect();
    f.render_widget(
        Paragraph::new(lines).style(Style::new().fg(theme.border).bg(theme.bar_bg)),
        Rect::new(x, area.y, 1, area.height),
    );
}

pub fn render_tab_bar(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    icon: &str,
    tabs: &[TabInfo],
    active: usize,
    hits: &mut HitMap<ChromeTarget>,
) {
    fill(f, area, Style::new().bg(theme.bar_bg));
    let tall = area.height >= 3;
    let y = mid(area);
    let closable = tabs.len() > 1;
    let plus_w = 7u16;
    let avail = area.width.saturating_sub(plus_w);
    let close_w: u16 = if closable { 4 } else { 1 };

    let labels: Vec<String> = tabs.iter().map(|t| format!("  {icon}  {}", truncate(&t.title, MAX_TITLE))).collect();
    let busy_w = |i: usize| if tabs[i].running > 0 { 2 + tabs[i].running.to_string().len() as u16 } else { 0 };
    let widths: Vec<u16> = labels.iter().enumerate().map(|(i, l)| l.width() as u16 + busy_w(i) + close_w + 1).collect();

    // Drop tabs from the left until the active one fits.
    let mut first = 0;
    while first < active && widths[first..=active].iter().sum::<u16>() > avail {
        first += 1;
    }

    let mut x = area.x;
    for (i, label) in labels.iter().enumerate().skip(first) {
        let w = widths[i];
        if x + w > area.x + avail {
            break;
        }
        let on = i == active;
        let bg = if on { theme.bg } else { theme.bar_bg };
        let rect = Rect::new(x, area.y, w - 1, area.height);
        fill(f, rect, Style::new().bg(bg));
        let fg = if on { theme.fg } else { theme.fg_muted };
        if tall {
            if on {
                // Accent line on top of the active tab.
                let line = "▔".repeat(rect.width as usize);
                f.render_widget(Paragraph::new(Span::styled(line, Style::new().fg(theme.accent).bg(bg))), Rect::new(x, area.y, rect.width, 1));
            }
        } else {
            let marker = if on { theme.accent } else { bg };
            text_at(f, x, y, vec![Span::styled("▎", Style::new().fg(marker).bg(bg))]);
        }
        let mut label_style = Style::new().fg(fg).bg(bg);
        if on {
            label_style = label_style.add_modifier(Modifier::BOLD);
        }
        let mut spans = vec![Span::styled(label.clone(), label_style)];
        if tabs[i].running > 0 {
            spans.push(Span::styled(format!(" ●{}", tabs[i].running), Style::new().fg(theme.warn).bg(bg)));
        }
        let lw = text_at(f, x, y, spans);
        hits.push(Rect::new(x, area.y, lw, area.height), ChromeTarget::Tab(i));
        if closable {
            let cx = x + lw + 1;
            text_at(f, cx, y, vec![Span::styled("×", Style::new().fg(theme.fg_muted).bg(bg))]);
            hits.push(Rect::new(cx - 1, area.y, 3, area.height), ChromeTarget::CloseTab(i));
        }
        x += w - 1;
        vline(f, x, area, theme);
        x += 1;
    }

    let plus = Rect::new(area.right().saturating_sub(plus_w), area.y, plus_w, area.height);
    vline(f, plus.x, area, theme);
    text_at(f, plus.x + 3, y, vec![Span::styled("+", Style::new().fg(theme.fg).bg(theme.bar_bg).add_modifier(Modifier::BOLD))]);
    hits.push(Rect::new(plus.x + 1, plus.y, plus_w - 1, plus.height), ChromeTarget::NewTab);
}

/// What the terminal header shows.
pub struct TermHeader<'a> {
    pub tabs: &'a [crate::modes::terminal::TermTabInfo],
    pub active: usize,
    pub focused: bool,
    pub size: TermSize,
}

const MAX_TERM_LABEL: usize = 28;

/// `▾ TERMINAL │ ⌂ ~/apps │ tail -f app.log ● × │ npm test ✓ × │ + ……… min  half  full`
pub fn render_term_header(f: &mut Frame, area: Rect, theme: &Theme, h: &TermHeader, hits: &mut HitMap<ChromeTarget>) {
    use crate::modes::terminal::TermState;
    if area.height == 0 {
        return;
    }
    fill(f, area, Style::new().bg(theme.bar_bg));
    let y = area.y;
    let base = Style::new().bg(theme.bar_bg);
    let sep = |f: &mut Frame, x: u16| text_at(f, x, y, vec![Span::styled("│", base.fg(theme.border))]);

    // Size buttons on the right: the current one is highlighted.
    let buttons = [(TermSize::Min, " min "), (TermSize::Normal, " half "), (TermSize::Full, " full ")];
    let buttons_w: u16 = buttons.iter().map(|(_, l)| l.width() as u16 + 1).sum::<u16>() + 1;
    let mut bx = area.right().saturating_sub(buttons_w);
    let limit = bx.saturating_sub(1);
    for (size, label) in buttons {
        let style = if size == h.size {
            base.bg(theme.accent_tint).fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            base.fg(theme.fg_muted)
        };
        let w = text_at(f, bx, y, vec![Span::styled(label, style)]);
        hits.push(Rect::new(bx, y, w, 1), ChromeTarget::TermSize(size));
        bx += w + 1;
    }

    let arrow = if h.size == TermSize::Min { "▸" } else { "▾" };
    let title_style = if h.focused { base.fg(theme.accent).add_modifier(Modifier::BOLD) } else { base.fg(theme.fg_muted) };
    let tw = text_at(f, area.x, y, vec![Span::styled(format!(" {arrow} TERMINAL "), title_style)]);
    hits.push(Rect::new(area.x, y, tw, 1), ChromeTarget::TermTitle);
    let mut x = area.x + tw;
    x += sep(f, x);

    // Terminal tabs: (spans, width) each; `+` after them.
    let tab_spans = |i: usize| -> Vec<Span<'static>> {
        let t = &h.tabs[i];
        let on = i == h.active;
        let bg = if on { theme.bg } else { theme.bar_bg };
        let mut label = Style::new().bg(bg).fg(if on { theme.fg } else { theme.fg_muted });
        if on {
            label = label.add_modifier(Modifier::BOLD);
        }
        let mut spans = vec![Span::styled(" ", label)];
        if t.nav {
            spans.push(Span::styled("⌂ ", Style::new().bg(bg).fg(theme.accent)));
        }
        // Folders keep their end (`…/apps/api`), commands their start (`tail -f app…`).
        let text = if t.nav { truncate_start(&t.label, MAX_TERM_LABEL) } else { truncate(&t.label, MAX_TERM_LABEL) };
        spans.push(Span::styled(text, label));
        match t.state {
            TermState::Running => spans.push(Span::styled(" ●", Style::new().bg(bg).fg(theme.warn))),
            TermState::Done(Some(0)) => spans.push(Span::styled(" ✓", Style::new().bg(bg).fg(theme.accent))),
            TermState::Done(Some(code)) => spans.push(Span::styled(format!(" ✗ {code}"), Style::new().bg(bg).fg(theme.danger))),
            TermState::Done(None) => spans.push(Span::styled(" ■", Style::new().bg(bg).fg(theme.fg_muted))),
            TermState::Idle => {}
        }
        spans.push(Span::styled(" ", label));
        spans
    };
    let close_w = |i: usize| if h.tabs[i].nav { 0 } else { 2 };
    let width = |spans: &[Span]| spans.iter().map(|s| s.width() as u16).sum::<u16>();
    let widths: Vec<u16> = (0..h.tabs.len()).map(|i| width(&tab_spans(i)) + close_w(i) + 1).collect();
    const PLUS_W: u16 = 4;
    let room = limit.saturating_sub(x + PLUS_W);
    // The navigation shell always shows; skip command tabs after it until the active one fits.
    let mut skip = 0;
    let shown = |skip: usize| -> u16 {
        widths[0] + if h.active == 0 { 0 } else { widths[(1 + skip).min(h.active)..=h.active].iter().sum::<u16>() }
    };
    while h.active > 0 && 1 + skip < h.active && shown(skip) > room {
        skip += 1;
    }
    let order: Vec<usize> = std::iter::once(0).chain((1 + skip)..h.tabs.len()).collect();
    for i in order {
        if x + widths[i] > limit.saturating_sub(PLUS_W) {
            break;
        }
        let spans = tab_spans(i);
        let w = text_at(f, x, y, spans);
        hits.push(Rect::new(x, y, w, 1), ChromeTarget::Term(i));
        x += w;
        if !h.tabs[i].nav {
            let bg = if i == h.active { theme.bg } else { theme.bar_bg };
            text_at(f, x, y, vec![Span::styled("× ", Style::new().bg(bg).fg(theme.fg_muted))]);
            hits.push(Rect::new(x, y, 2, 1), ChromeTarget::CloseTerm(i));
            x += 2;
        }
        x += sep(f, x);
    }
    if x + PLUS_W <= limit {
        text_at(f, x, y, vec![Span::styled(" + ", base.fg(theme.fg).add_modifier(Modifier::BOLD))]);
        hits.push(Rect::new(x, y, 3, 1), ChromeTarget::NewTerm);
    }
}

/// Status on the left, then `Ctrl-] Shortcuts │ Ctrl-q Quit` on the right.
pub fn render_bottom_bar(f: &mut Frame, area: Rect, theme: &Theme, status: &str, leader: &str, hits: &mut HitMap<ChromeTarget>) {
    fill(f, area, Style::new().bg(theme.bar_bg));
    let y = mid(area);
    let keycap = |k: &str| Span::styled(format!(" {k} "), Style::new().bg(theme.fg_muted).fg(theme.bar_bg).add_modifier(Modifier::BOLD));
    let label = |l: &str| Span::styled(format!(" {l}"), Style::new().fg(theme.fg).bg(theme.bar_bg));
    let groups = [
        (vec![keycap(leader), label("Shortcuts")], ChromeTarget::Shortcuts),
        (vec![keycap("Ctrl-q"), label("Quit")], ChromeTarget::Quit),
    ];

    const SEP: u16 = 5; // "  │  "
    let width = |spans: &[Span]| spans.iter().map(|s| s.width() as u16).sum::<u16>();
    let mut right = area.right().saturating_sub(2);
    let min_x = area.x + 2;
    let n = groups.len();
    for (i, (spans, target)) in groups.into_iter().enumerate().rev() {
        let w = width(&spans);
        let sep = if i + 1 < n { SEP } else { 0 };
        if right < min_x + w + sep {
            break;
        }
        if sep > 0 {
            right -= sep;
            text_at(f, right + 2, y, vec![Span::styled("│", Style::new().fg(theme.border).bg(theme.bar_bg))]);
        }
        right -= w;
        text_at(f, right, y, spans);
        hits.push(Rect::new(right.saturating_sub(1), area.y, w + 2, area.height), target);
    }

    // Status takes what is left on the left, truncated from the end.
    let room = right.saturating_sub(min_x + SEP) as usize;
    if room > 3 {
        let text = truncate(status, room);
        text_at(f, min_x, y, vec![Span::styled(text, Style::new().fg(theme.fg_muted).bg(theme.bar_bg))]);
    }
}
