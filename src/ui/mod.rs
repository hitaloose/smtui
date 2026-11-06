pub mod bars;
pub mod hit;
pub mod menu;
pub mod terminal_guard;
pub mod theme;
pub mod toast;

use ratatui::layout::Rect;

/// Screen regions, top to bottom.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppLayout {
    pub tab_bar: Rect,
    pub toolbar: Rect,
    pub content: Rect,
    pub bottom_bar: Rect,
}

impl AppLayout {
    /// Bars are 3 rows tall like the prototype's 44px controls; below `COMPACT_BELOW`
    /// rows everything shrinks to a single row to leave room for content.
    pub fn new(area: Rect, toolbar_height: u16) -> Self {
        const COMPACT_BELOW: u16 = 20;
        let compact = area.height < COMPACT_BELOW;
        let bar = if compact { 1 } else { 3 };
        let row = |y: u16, h: u16| Rect::new(area.x, y, area.width, h);
        let tab_h = area.height.min(bar);
        let bottom_h = area.height.saturating_sub(tab_h).min(bar);
        let middle = area.height - tab_h - bottom_h;
        let tool_h = if compact { toolbar_height.min(1) } else { toolbar_height }.min(middle);
        Self {
            tab_bar: row(area.y, tab_h),
            toolbar: row(area.y + tab_h, tool_h),
            content: row(area.y + tab_h + tool_h, middle - tool_h),
            bottom_bar: row(area.bottom() - bottom_h, bottom_h),
        }
    }
}
