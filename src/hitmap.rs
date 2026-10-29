//! Screen regions recorded while drawing, so mouse events can be mapped to what is
//! under the pointer.

use ratatui::layout::{Position, Rect};

use crate::event::PaneId;
use crate::keymap::Action;

#[derive(Debug, Clone, Default)]
pub struct PaneArea {
    /// The file list's window, border included.
    pub frame: Rect,
    /// The address bar's window, border included.
    pub bar: Rect,
    pub title: Rect,
    /// The button box right of the address bar (reload, or go while editing).
    pub button: Rect,
    /// Where Go to edits the address: the edit window's inside, or the title.
    pub edit: Rect,
    pub list: Rect,
    /// First visible row of the pane's view.
    pub offset: usize,
    /// Number of rows in the pane's view.
    pub rows: usize,
}

#[derive(Debug, Clone, Default)]
pub struct HitMap {
    pub panes: [PaneArea; 2],
    pub queue: Rect,
    /// Screen row, job index in the queue, and the column of its cancel mark.
    pub queue_rows: Vec<(u16, usize, u16)>,
    pub hints: Vec<(Rect, Action)>,
    pub dialog: Option<Rect>,
    pub buttons: Vec<Rect>,
    /// A list inside a dialog (the remote picker): its area and first visible row.
    pub dialog_list: Option<(Rect, usize)>,
    /// How far the help or error log can scroll at the current size.
    pub scroll_max: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// Row `row` of a pane's view (row 0 may be `..`).
    Item {
        pane: PaneId,
        row: usize,
    },
    /// Empty space below a pane's last row.
    Blank {
        pane: PaneId,
    },
    /// The address bar.
    Title {
        pane: PaneId,
    },
    /// A button in a pane's title; the action applies to that pane.
    PaneButton {
        pane: PaneId,
        action: Action,
    },
    QueueRow {
        index: usize,
        cancel: bool,
    },
    Queue,
    Hint(Action),
    Button(usize),
    DialogRow(usize),
    /// Inside a dialog, not on anything clickable.
    Dialog,
    Nothing,
}

impl HitMap {
    pub fn hit(&self, col: u16, row: u16) -> Target {
        let p = Position::new(col, row);
        if let Some(dialog) = self.dialog {
            if let Some(i) = self.buttons.iter().position(|b| b.contains(p)) {
                return Target::Button(i);
            }
            if let Some((area, offset)) = self.dialog_list
                && area.contains(p)
            {
                return Target::DialogRow(offset + usize::from(row - area.y));
            }
            return if dialog.contains(p) { Target::Dialog } else { Target::Nothing };
        }
        for (pane, area) in self.panes.iter().enumerate() {
            if area.list.contains(p) {
                let r = area.offset + usize::from(row - area.list.y);
                return if r < area.rows {
                    Target::Item { pane, row: r }
                } else {
                    Target::Blank { pane }
                };
            }
            if area.button.contains(p) {
                return Target::PaneButton { pane, action: Action::Refresh };
            }
            if area.bar.contains(p) {
                return Target::Title { pane };
            }
            // The rest of the border: the pane, like the space below its entries.
            if area.frame.contains(p) {
                return Target::Blank { pane };
            }
        }
        if let Some(&(_, index, cancel_x)) = self.queue_rows.iter().find(|(y, _, _)| *y == row) {
            return Target::QueueRow { index, cancel: col == cancel_x };
        }
        if self.queue.contains(p) {
            return Target::Queue;
        }
        if let Some((_, action)) = self.hints.iter().find(|(r, _)| r.contains(p)) {
            return Target::Hint(*action);
        }
        Target::Nothing
    }

    /// The pane whose list area contains `row` and `col`.
    pub fn pane_at(&self, col: u16, row: u16) -> Option<PaneId> {
        let p = Position::new(col, row);
        self.panes.iter().position(|a| {
            a.frame.contains(p) || a.list.contains(p) || a.title.contains(p) || a.bar.contains(p)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> HitMap {
        let mut m = HitMap::default();
        m.panes[0] = PaneArea {
            title: Rect::new(0, 0, 10, 1),
            button: Rect::new(6, 0, 2, 1),
            edit: Rect::default(),
            frame: Rect::default(),
            bar: Rect::new(0, 0, 10, 1),
            list: Rect::new(0, 1, 10, 5),
            offset: 10,
            rows: 12,
        };
        m.panes[1].list = Rect::new(11, 1, 10, 5);
        m.queue = Rect::new(0, 7, 21, 2);
        m.queue_rows = vec![(8, 0, 20)];
        m.hints = vec![(Rect::new(0, 10, 8, 1), Action::Copy)];
        m
    }

    #[test]
    fn rows_account_for_scroll_offset() {
        let m = map();
        assert_eq!(m.hit(2, 1), Target::Item { pane: 0, row: 10 });
        assert_eq!(m.hit(2, 2), Target::Item { pane: 0, row: 11 });
        assert_eq!(m.hit(2, 3), Target::Blank { pane: 0 });
        assert_eq!(m.hit(12, 1), Target::Blank { pane: 1 });
    }

    #[test]
    fn title_queue_and_hints() {
        let m = map();
        assert_eq!(m.hit(2, 0), Target::Title { pane: 0 });
        assert_eq!(m.hit(7, 0), Target::PaneButton { pane: 0, action: Action::Refresh });
        assert_eq!(m.hit(8, 0), Target::Title { pane: 0 });
        assert_eq!(m.hit(3, 8), Target::QueueRow { index: 0, cancel: false });
        assert_eq!(m.hit(20, 8), Target::QueueRow { index: 0, cancel: true });
        assert_eq!(m.hit(3, 7), Target::Queue);
        assert_eq!(m.hit(3, 10), Target::Hint(Action::Copy));
        assert_eq!(m.hit(30, 30), Target::Nothing);
    }

    #[test]
    fn dialog_captures_everything() {
        let mut m = map();
        m.dialog = Some(Rect::new(2, 2, 10, 5));
        m.buttons = vec![Rect::new(3, 5, 4, 1)];
        m.dialog_list = Some((Rect::new(3, 3, 8, 2), 4));
        assert_eq!(m.hit(4, 5), Target::Button(0));
        assert_eq!(m.hit(4, 4), Target::DialogRow(5));
        assert_eq!(m.hit(2, 2), Target::Dialog);
        assert_eq!(m.hit(2, 1), Target::Nothing);
    }
}

/// Proofs over every input, checked by `cargo kani` (see SECURITY.md).
#[cfg(kani)]
mod proofs {
    use super::*;

    fn any_rect() -> Rect {
        Rect::new(kani::any(), kani::any(), kani::any(), kani::any())
    }

    /// For any screen position and any layout the drawing recorded, mapping a click never
    /// overflows; a row in a list is the row clicked.
    #[kani::proof]
    // The longest loop is over the two panes; the other lists stay empty.
    #[kani::unwind(3)]
    fn every_click_maps_without_overflow() {
        let mut map = HitMap::default();
        let offset: usize = kani::any_where(|&o| o <= usize::from(u16::MAX));
        if kani::any() {
            map.dialog = Some(any_rect());
            map.dialog_list = Some((any_rect(), offset));
        }
        map.panes[0].list = any_rect();
        map.panes[0].offset = offset;
        map.panes[0].rows = kani::any();
        let (col, row) = (kani::any(), kani::any());
        if let Target::Item { pane: 0, row: r } = map.hit(col, row) {
            assert!(r >= offset && r < map.panes[0].rows);
        }
    }
}
