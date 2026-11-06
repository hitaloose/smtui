//! Hit-testing for mouse clicks. Ratatui has no notion of clickable widgets, so every
//! interactive element records its `Rect` while rendering and clicks are matched here.

use ratatui::layout::{Position, Rect};

#[derive(Debug, Clone)]
pub struct HitMap<T> {
    targets: Vec<(Rect, T)>,
}

impl<T> Default for HitMap<T> {
    fn default() -> Self {
        Self { targets: Vec::new() }
    }
}

impl<T: Clone> HitMap<T> {
    /// Call at the start of every frame.
    pub fn clear(&mut self) {
        self.targets.clear();
    }

    pub fn push(&mut self, rect: Rect, target: T) {
        if rect.width > 0 && rect.height > 0 {
            self.targets.push((rect, target));
        }
    }

    /// The most recently pushed target under `(x, y)`, so elements drawn later win.
    pub fn hit(&self, x: u16, y: u16) -> Option<T> {
        let pos = Position::new(x, y);
        self.targets
            .iter()
            .rev()
            .find(|(r, _)| r.contains(pos))
            .map(|(_, t)| t.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_targets_win() {
        let mut m = HitMap::default();
        m.push(Rect::new(0, 0, 10, 10), 1);
        m.push(Rect::new(2, 2, 2, 2), 2);
        assert_eq!(m.hit(3, 3), Some(2));
        assert_eq!(m.hit(0, 0), Some(1));
        assert_eq!(m.hit(20, 0), None);
    }
}
