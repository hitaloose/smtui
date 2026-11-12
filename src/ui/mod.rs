pub mod bars;
pub mod dialog;
pub mod hit;
pub mod menu;
pub mod run_dialog;
pub mod terminal_guard;
pub mod theme;
pub mod toast;

use ratatui::layout::Rect;

/// How much room the terminal of a tab takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermSize {
    /// Only its header row.
    Min,
    /// About half of the space between the bars.
    Normal,
    /// Covers the Files pane.
    Full,
}

impl TermSize {
    /// Min → Normal → Full → Min.
    pub fn next(self) -> Self {
        match self {
            TermSize::Min => TermSize::Normal,
            TermSize::Normal => TermSize::Full,
            TermSize::Full => TermSize::Min,
        }
    }
}

/// Screen regions, top to bottom. Hidden regions have a height of 0.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppLayout {
    pub tab_bar: Rect,
    pub toolbar: Rect,
    pub files: Rect,
    pub term_header: Rect,
    pub term: Rect,
    pub bottom_bar: Rect,
}

impl AppLayout {
    /// Bars are 3 rows tall like the prototype's 44px controls; below `COMPACT_BELOW`
    /// rows everything shrinks to a single row to leave room for content. The terminal
    /// header is always one row.
    pub fn new(area: Rect, toolbar_height: u16, size: TermSize) -> Self {
        const COMPACT_BELOW: u16 = 20;
        let compact = area.height < COMPACT_BELOW;
        let bar = if compact { 1 } else { 3 };
        let row = |y: u16, h: u16| Rect::new(area.x, y, area.width, h);
        let tab_h = area.height.min(bar);
        let bottom_h = area.height.saturating_sub(tab_h).min(bar);
        let middle = area.height - tab_h - bottom_h;
        let header_h = middle.min(1);
        let below_header = middle - header_h;
        // Rows for the Files pane (toolbar + list) and for the terminal body.
        let (files_h, term_h) = match size {
            TermSize::Min => (below_header, 0),
            TermSize::Full => (0, below_header),
            TermSize::Normal => {
                let files = below_header / 2;
                (files, below_header - files)
            }
        };
        let tool_h = if compact { toolbar_height.min(1) } else { toolbar_height }.min(files_h);
        let top = area.y + tab_h;
        Self {
            tab_bar: row(area.y, tab_h),
            toolbar: row(top, tool_h),
            files: row(top + tool_h, files_h - tool_h),
            term_header: row(top + files_h, header_h),
            term: row(top + files_h + header_h, term_h),
            bottom_bar: row(area.bottom() - bottom_h, bottom_h),
        }
    }

    /// The Files pane, toolbar included.
    pub fn files_pane(&self) -> Rect {
        self.toolbar.union(self.files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total(l: &AppLayout) -> u16 {
        l.tab_bar.height + l.toolbar.height + l.files.height + l.term_header.height + l.term.height + l.bottom_bar.height
    }

    #[test]
    fn normal_splits_in_half() {
        let l = AppLayout::new(Rect::new(0, 0, 100, 41), 3, TermSize::Normal);
        assert_eq!(total(&l), 41);
        assert_eq!(l.toolbar.height + l.files.height, 17);
        assert_eq!(l.term.height, 17);
        assert_eq!(l.term_header.y, l.files.bottom());
        assert_eq!(l.term.bottom(), l.bottom_bar.y);
    }

    #[test]
    fn min_and_full() {
        let min = AppLayout::new(Rect::new(0, 0, 100, 40), 3, TermSize::Min);
        assert_eq!((min.term.height, min.term_header.height), (0, 1));
        assert_eq!(min.term_header.bottom(), min.bottom_bar.y);
        assert_eq!(total(&min), 40);
        let full = AppLayout::new(Rect::new(0, 0, 100, 40), 3, TermSize::Full);
        assert_eq!((full.toolbar.height, full.files.height), (0, 0));
        assert_eq!(full.term_header.y, full.tab_bar.bottom());
        assert_eq!(total(&full), 40);
    }

    #[test]
    fn tiny_screens_do_not_overflow() {
        for h in 0..12 {
            for size in [TermSize::Min, TermSize::Normal, TermSize::Full] {
                let l = AppLayout::new(Rect::new(0, 0, 50, h), 3, size);
                assert_eq!(total(&l), h, "height {h} {size:?}");
            }
        }
    }
}
