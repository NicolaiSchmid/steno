//! Where a floating panel sits: rectangles in logical points (origin
//! top-left, Tauri's convention, not `AppKit`'s), the anchor both panels
//! hang from, the frame a panel of a given size takes under it, and what
//! a page's size report may ask for. Plain values and plain functions;
//! `panels.rs` applies them to windows.
//!
//! Swift: `FloatingPanelModel.swift` (the anchor and its validation),
//! `FloatingPanel.swift` (`contentSizeDidChange`).

use serde::{Deserialize, Serialize};

/// `Theme.Space.sm`: the default anchor's distance from the screen's top.
pub const DEFAULT_TOP_INSET: f64 = 8.0;

/// A rectangle in logical points, origin top-left.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn mid_x(&self) -> f64 {
        self.x + self.width / 2.0
    }

    pub fn max_x(&self) -> f64 {
        self.x + self.width
    }

    pub fn max_y(&self) -> f64 {
        self.y + self.height
    }

    pub fn contains_point(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.max_x() && y >= self.y && y < self.max_y()
    }

    /// Whether `other` lies wholly inside, edges included.
    pub fn contains(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.max_x() <= self.max_x()
            && other.max_y() <= self.max_y()
    }

    /// The screen among `screens` that holds the point, else `fallback`.
    pub fn holding(screens: &[Rect], point: (f64, f64), fallback: Rect) -> Rect {
        screens
            .iter()
            .copied()
            .find(|screen| screen.contains_point(point.0, point.1))
            .unwrap_or(fallback)
    }
}

/// Where the panel hangs: the top-centre point of its frame and the
/// visible frame of the screen it was saved on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PanelAnchor {
    pub top_center: (f64, f64),
    pub screen: Rect,
}

impl PanelAnchor {
    /// The default on a screen's visible frame: top centre, `DEFAULT_TOP_INSET`
    /// under the top edge.
    pub fn default_in(screen: Rect) -> Self {
        Self {
            top_center: (screen.mid_x(), screen.y + DEFAULT_TOP_INSET),
            screen,
        }
    }

    /// The frame of a panel of `size` hanging from the anchor.
    pub fn frame_for(&self, size: (f64, f64)) -> Rect {
        frame_hanging_from(self.top_center, size)
    }

    /// The anchor that describes a panel at `frame`, on the screen among
    /// `screens` that holds its top-centre point (else `fallback`).
    pub fn from_frame(frame: Rect, screens: &[Rect], fallback: Rect) -> Self {
        let point = (frame.mid_x(), frame.y);
        Self {
            top_center: point,
            screen: Rect::holding(screens, point, fallback),
        }
    }

    /// A saved anchor is kept while a panel of `size` hanging from it lies
    /// within one of the current screens; otherwise the default on
    /// `fallback`. Before the page has measured, `size` is `PROBE_SIZE`
    /// and only the anchor point itself is checked.
    pub fn validated(
        saved: Option<Self>,
        size: (f64, f64),
        screens: &[Rect],
        fallback: Rect,
    ) -> Self {
        if let Some(saved) = saved
            && screens
                .iter()
                .any(|screen| screen.contains(&saved.frame_for(size)))
        {
            return saved;
        }
        Self::default_in(fallback)
    }
}

/// The size a saved anchor is validated with before its panel's page has
/// measured: one point each way, so a panel that will fit is not thrown
/// back to the default because the width before measuring is the maximum.
pub const PROBE_SIZE: (f64, f64) = (1.0, 1.0);

/// The frame of a panel of `size` whose top-centre point is `top_center`,
/// on whole points.
pub fn frame_hanging_from(top_center: (f64, f64), size: (f64, f64)) -> Rect {
    Rect::new(
        (top_center.0 - size.0 / 2.0).round(),
        top_center.1.round(),
        size.0,
        size.1,
    )
}

/// Two sizes within a point of each other are the same size (the window
/// system rounds to the pixel grid).
pub fn same_size(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1.0 && (a.1 - b.1).abs() < 1.0
}

/// The size a panel takes from its page's report: finite and at least a
/// point each way (a sub-point, zero or negative size is no size), clamped
/// to the work area it hangs in, so a report of a million points cannot
/// grow the window past its screen.
pub fn accepted_size(reported: (f64, f64), work_area: (f64, f64)) -> Option<(f64, f64)> {
    let is_size = |value: f64| value.is_finite() && value >= 1.0;
    (is_size(reported.0) && is_size(reported.1))
        .then(|| (reported.0.min(work_area.0), reported.1.min(work_area.1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect::new(0.0, 25.0, 1440.0, 875.0);
    const SECOND: Rect = Rect::new(1440.0, 0.0, 1920.0, 1080.0);

    #[test]
    fn the_default_anchor_is_top_centre_under_the_top_edge() {
        let anchor = PanelAnchor::default_in(SCREEN);
        assert_eq!(anchor.top_center, (720.0, 33.0));
        assert_eq!(anchor.screen, SCREEN);
        let frame = anchor.frame_for((480.0, 56.0));
        assert_eq!(frame, Rect::new(480.0, 33.0, 480.0, 56.0));
        // Both contents hang from the same point.
        let bubble = anchor.frame_for((240.0, 40.0));
        assert_eq!(bubble.mid_x(), frame.mid_x());
        assert_eq!(bubble.y, frame.y);
    }

    #[test]
    fn a_resize_keeps_the_top_centre() {
        let before = Rect::new(1160.0, 700.0, 480.0, 56.0);
        let after = frame_hanging_from((before.mid_x(), before.y), (384.0, 56.0));
        assert_eq!(after, Rect::new(1208.0, 700.0, 384.0, 56.0));
        assert_eq!(after.mid_x(), before.mid_x());
        let taller = frame_hanging_from((after.mid_x(), after.y), (384.0, 68.0));
        assert_eq!((taller.x, taller.y), (after.x, after.y));
    }

    #[test]
    fn frames_land_on_whole_points() {
        let anchor = PanelAnchor {
            top_center: (100.3, 20.6),
            screen: SCREEN,
        };
        let frame = anchor.frame_for((33.0, 40.0));
        assert_eq!((frame.x, frame.y), (84.0, 21.0));
    }

    #[test]
    fn a_dragged_frame_becomes_the_anchor_on_its_screen() {
        let frame = Rect::new(1500.0, 100.0, 240.0, 40.0);
        let anchor = PanelAnchor::from_frame(frame, &[SCREEN, SECOND], SCREEN);
        assert_eq!(anchor.top_center, (1620.0, 100.0));
        assert_eq!(anchor.screen, SECOND);
        // Off every screen: the fallback is recorded as the screen.
        let off =
            PanelAnchor::from_frame(Rect::new(-500.0, -500.0, 240.0, 40.0), &[SCREEN], SCREEN);
        assert_eq!(off.screen, SCREEN);
    }

    #[test]
    fn a_saved_anchor_survives_while_its_panel_fits_a_current_screen() {
        let saved = PanelAnchor {
            top_center: (1620.0, 100.0),
            screen: SECOND,
        };
        assert_eq!(
            PanelAnchor::validated(Some(saved), (240.0, 40.0), &[SCREEN, SECOND], SCREEN),
            saved
        );
        // The second screen is gone: back to the default on the main one.
        assert_eq!(
            PanelAnchor::validated(Some(saved), (240.0, 40.0), &[SCREEN], SCREEN),
            PanelAnchor::default_in(SCREEN)
        );
        // A panel whose bottom would hang below the screen does not fit,
        // even though its anchor point lies inside.
        let low = PanelAnchor {
            top_center: (720.0, 880.0),
            screen: SCREEN,
        };
        assert_eq!(
            PanelAnchor::validated(Some(low), (240.0, 40.0), &[SCREEN], SCREEN),
            PanelAnchor::default_in(SCREEN)
        );
        assert_eq!(
            PanelAnchor::validated(None, (240.0, 40.0), &[SCREEN], SCREEN),
            PanelAnchor::default_in(SCREEN)
        );
    }

    /// A bubble dragged to the right edge fits as the 240-point pill it
    /// is; before the page has measured, the probe keeps it there where
    /// the 480-point maximum would have thrown it back to the default.
    #[test]
    fn the_probe_keeps_an_anchor_the_unmeasured_width_would_reject() {
        let near_edge = PanelAnchor {
            top_center: (1300.0, 100.0),
            screen: SCREEN,
        };
        assert_eq!(
            PanelAnchor::validated(Some(near_edge), PROBE_SIZE, &[SCREEN], SCREEN),
            near_edge
        );
        assert_eq!(
            PanelAnchor::validated(Some(near_edge), (240.0, 40.0), &[SCREEN], SCREEN),
            near_edge
        );
        assert_eq!(
            PanelAnchor::validated(Some(near_edge), (480.0, 56.0), &[SCREEN], SCREEN),
            PanelAnchor::default_in(SCREEN)
        );
        // The probe still rejects a point that is on no screen.
        let gone = PanelAnchor {
            top_center: (1620.0, 100.0),
            screen: SECOND,
        };
        assert_eq!(
            PanelAnchor::validated(Some(gone), PROBE_SIZE, &[SCREEN], SCREEN),
            PanelAnchor::default_in(SCREEN)
        );
    }

    #[test]
    fn the_anchor_round_trips_through_json() {
        let anchor = PanelAnchor {
            top_center: (720.0, 33.0),
            screen: SCREEN,
        };
        let json = serde_json::to_string(&anchor).unwrap();
        assert_eq!(serde_json::from_str::<PanelAnchor>(&json).unwrap(), anchor);
    }

    #[test]
    fn sub_point_differences_are_the_same_size() {
        assert!(same_size((240.0, 40.0), (240.4, 39.6)));
        assert!(!same_size((240.0, 40.0), (241.0, 40.0)));
        assert!(!same_size((240.0, 40.0), (240.0, 68.0)));
    }

    #[test]
    fn rects_contain_points_and_rects() {
        assert!(SCREEN.contains_point(0.0, 25.0));
        assert!(!SCREEN.contains_point(1440.0, 25.0));
        assert!(SCREEN.contains(&Rect::new(0.0, 25.0, 1440.0, 875.0)));
        assert!(!SCREEN.contains(&Rect::new(0.0, 24.0, 10.0, 10.0)));
        assert_eq!(SCREEN.max_y(), 900.0);
        assert_eq!(
            Rect::holding(&[SCREEN, SECOND], (1500.0, 10.0), SCREEN),
            SECOND
        );
        assert_eq!(Rect::holding(&[SCREEN], (-1.0, -1.0), SECOND), SECOND);
    }

    #[test]
    fn a_report_is_a_size_only_when_finite_and_at_least_a_point() {
        const AREA: (f64, f64) = (1440.0, 875.0);
        assert_eq!(accepted_size((244.5, 40.0), AREA), Some((244.5, 40.0)));
        assert_eq!(accepted_size((1.0, 1.0), AREA), Some((1.0, 1.0)));
        for bad in [
            (0.0, 40.0),
            (240.0, 0.0),
            (-240.0, 40.0),
            (240.0, -1.0),
            (0.5, 40.0),
            (1e-300, 40.0),
            (f64::NAN, 40.0),
            (240.0, f64::NAN),
            (f64::INFINITY, 40.0),
            (240.0, f64::NEG_INFINITY),
        ] {
            assert_eq!(accepted_size(bad, AREA), None, "{bad:?}");
        }
    }

    #[test]
    fn a_report_is_clamped_to_the_work_area() {
        const AREA: (f64, f64) = (1440.0, 875.0);
        assert_eq!(accepted_size((1e9, 40.0), AREA), Some((1440.0, 40.0)));
        assert_eq!(accepted_size((240.0, 1e9), AREA), Some((240.0, 875.0)));
        assert_eq!(accepted_size((1440.0, 875.0), AREA), Some((1440.0, 875.0)));
        assert_eq!(accepted_size((1441.0, 876.0), AREA), Some((1440.0, 875.0)));
    }
}
