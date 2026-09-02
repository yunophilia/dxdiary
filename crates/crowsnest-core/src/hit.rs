//! Mouse hit testing.
//!
//! ratatui has no hit testing of its own — crossterm hands you a column and a
//! row, and mapping that to a widget is the application's problem. A code pane
//! also needs character precision, not just "which widget", so a generic
//! widget-region crate does not cover it.
//!
//! The model: rendering rebuilds the map every frame, recording the rect each
//! interactive region occupies. Event handling resolves a click against it.
//! Regions pushed later win, so overlays and popups shadow what is beneath
//! them for free.
//!
//! Areas register once as a whole rather than per character. A [`Hit`] carries
//! coordinates local to the region, so the caller converts to a line and column
//! with its own scroll offset — the map stays small and scroll-independent.

use ratatui::layout::{Position, Rect};

/// Something the user can click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    /// Body of the file tree. Local row + scroll = index of the visible row.
    TreeBody,
    /// Body of the content pane. Local position + scroll = line and column.
    ContentBody,
    /// A pane header — clicking focuses that pane.
    Header(PaneId),
    /// The draggable divider between panes.
    Divider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneId {
    Tree,
    Content,
}

/// A resolved click: what was hit, and where within it.
#[derive(Debug, Clone, Copy)]
pub struct Hit {
    pub target: HitTarget,
    /// Column within the region, 0-based.
    pub local_col: u16,
    /// Row within the region, 0-based.
    pub local_row: u16,
}

#[derive(Debug, Default)]
pub struct HitMap {
    regions: Vec<(Rect, HitTarget)>,
}

impl HitMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop everything. Call at the start of each frame.
    pub fn clear(&mut self) {
        self.regions.clear();
    }

    /// Register a clickable region. Later calls shadow earlier ones.
    pub fn push(&mut self, rect: Rect, target: HitTarget) {
        self.regions.push((rect, target));
    }

    /// Resolve a terminal coordinate, topmost region first.
    pub fn resolve(&self, col: u16, row: u16) -> Option<Hit> {
        let pos = Position::new(col, row);
        self.regions
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(pos))
            .map(|(rect, target)| Hit {
                target: *target,
                local_col: col.saturating_sub(rect.x),
                local_row: row.saturating_sub(rect.y),
            })
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn resolves_local_coordinates_relative_to_the_region() {
        let mut map = HitMap::new();
        map.push(r(10, 5, 20, 8), HitTarget::ContentBody);

        let hit = map.resolve(13, 7).expect("inside the region");
        assert_eq!(hit.target, HitTarget::ContentBody);
        assert_eq!((hit.local_col, hit.local_row), (3, 2));
    }

    #[test]
    fn misses_outside_every_region() {
        let mut map = HitMap::new();
        map.push(r(0, 0, 5, 5), HitTarget::TreeBody);
        assert!(map.resolve(9, 9).is_none());
    }

    #[test]
    fn later_regions_shadow_earlier_ones() {
        let mut map = HitMap::new();
        map.push(r(0, 0, 20, 20), HitTarget::TreeBody);
        map.push(r(5, 5, 5, 5), HitTarget::ContentBody);

        // Inside the overlay.
        assert_eq!(map.resolve(6, 6).unwrap().target, HitTarget::ContentBody);
        // Outside it, but still inside the region beneath.
        assert_eq!(map.resolve(1, 1).unwrap().target, HitTarget::TreeBody);
    }

    #[test]
    fn boundaries_are_half_open() {
        let mut map = HitMap::new();
        map.push(r(0, 0, 4, 4), HitTarget::TreeBody);
        assert!(map.resolve(3, 3).is_some(), "last cell is inside");
        assert!(map.resolve(4, 4).is_none(), "one past the edge is outside");
    }

    #[test]
    fn clearing_drops_every_region() {
        let mut map = HitMap::new();
        map.push(r(0, 0, 4, 4), HitTarget::TreeBody);
        map.clear();
        assert!(map.is_empty());
        assert!(map.resolve(1, 1).is_none());
    }
}
