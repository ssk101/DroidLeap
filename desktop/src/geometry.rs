//! Desktop geometry: which edge, where the barriers go, where the pointer comes back.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    /// The tablet's edge that faces back towards this side of the desktop.
    pub fn opposite(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
        }
    }

    pub fn is_vertical(self) -> bool {
        matches!(self, Side::Left | Side::Right)
    }

    /// Edge coordinate of a monitor on this side, in the portal's convention
    /// (right/bottom barriers sit one past the last pixel).
    fn edge_of(self, r: &Rect) -> i32 {
        match self {
            Side::Left => r.x,
            Side::Right => r.x + r.width,
            Side::Top => r.y,
            Side::Bottom => r.y + r.height,
        }
    }

    /// Splits a motion delta into (push towards the outside, movement along the edge).
    pub fn split(self, dx: f64, dy: f64) -> (f64, f64) {
        match self {
            Side::Left => (-dx, dy),
            Side::Right => (dx, dy),
            Side::Top => (-dy, dx),
            Side::Bottom => (dy, dx),
        }
    }

    /// Cursor position `inward` pixels back inside the desktop and `along` along the edge.
    pub fn moved(self, (x, y): (f64, f64), inward: f64, along: f64) -> (f64, f64) {
        match self {
            Side::Left => (x + inward, y + along),
            Side::Right => (x - inward, y + along),
            Side::Top => (x + along, y + inward),
            Side::Bottom => (x + along, y - inward),
        }
    }
}

/// A monitor in desktop (logical) pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// A barrier along one monitor's outer edge. `line` is (x1, y1, x2, y2), end points inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeBarrier {
    pub id: u32,
    pub monitor: Rect,
    pub line: (i32, i32, i32, i32),
}

/// Barriers on every monitor whose edge on `side` is the outer edge of the whole desktop.
/// Ids start at 1, as the portal requires.
pub fn outer_barriers(monitors: &[Rect], side: Side) -> Vec<EdgeBarrier> {
    let edges = monitors.iter().map(|m| side.edge_of(m));
    let Some(extreme) = (match side {
        Side::Left | Side::Top => edges.min(),
        Side::Right | Side::Bottom => edges.max(),
    }) else {
        return Vec::new();
    };

    monitors
        .iter()
        .filter(|m| side.edge_of(m) == extreme)
        .zip(1u32..)
        .map(|(&m, id)| {
            let e = side.edge_of(&m);
            let line = if side.is_vertical() {
                (e, m.y, e, m.y + m.height - 1)
            } else {
                (m.x, e, m.x + m.width - 1, e)
            };
            EdgeBarrier {
                id,
                monitor: m,
                line,
            }
        })
        .collect()
}

/// Where the pointer reappears when the tablet hands back: one pixel inside
/// `monitor`'s `side` edge, `along` (0 to 1) of the way along it.
pub fn return_point(side: Side, monitor: &Rect, along: f64) -> (f64, f64) {
    let along = along.clamp(0.0, 1.0);
    let x_along = monitor.x as f64 + along * (monitor.width - 1) as f64;
    let y_along = monitor.y as f64 + along * (monitor.height - 1) as f64;
    match side {
        Side::Left => ((monitor.x + 1) as f64, y_along),
        Side::Right => ((monitor.x + monitor.width - 2) as f64, y_along),
        Side::Top => (x_along, (monitor.y + 1) as f64),
        Side::Bottom => (x_along, (monitor.y + monitor.height - 2) as f64),
    }
}

/// The barrier whose edge pixel row or column the cursor at (`x`, `y`) is on, or beyond.
pub fn touched_barrier(
    barriers: &[EdgeBarrier],
    side: Side,
    (x, y): (i32, i32),
) -> Option<&EdgeBarrier> {
    barriers.iter().find(|b| {
        let m = &b.monitor;
        let along_x = (m.x..m.x + m.width).contains(&x);
        let along_y = (m.y..m.y + m.height).contains(&y);
        match side {
            Side::Left => along_y && x <= m.x,
            Side::Right => along_y && x >= m.x + m.width - 1,
            Side::Top => along_x && y <= m.y,
            Side::Bottom => along_x && y >= m.y + m.height - 1,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// Two 1920x1080 monitors side by side, plus a smaller one below the left one.
    const DESKTOP: [Rect; 3] = [
        rect(0, 0, 1920, 1080),
        rect(1920, 0, 1920, 1080),
        rect(0, 1080, 1280, 720),
    ];

    #[test]
    fn bottom_barrier_only_on_the_lowest_monitor() {
        let barriers = outer_barriers(&DESKTOP, Side::Bottom);
        assert_eq!(
            barriers,
            vec![EdgeBarrier {
                id: 1,
                monitor: DESKTOP[2],
                line: (0, 1800, 1279, 1800)
            }]
        );
    }

    #[test]
    fn top_barriers_on_every_monitor_flush_with_the_top() {
        let lines: Vec<_> = outer_barriers(&DESKTOP, Side::Top)
            .iter()
            .map(|b| (b.id, b.line))
            .collect();
        assert_eq!(lines, vec![(1, (0, 0, 1919, 0)), (2, (1920, 0, 3839, 0))]);
    }

    #[test]
    fn right_barrier_sits_one_past_the_last_pixel() {
        let barriers = outer_barriers(&DESKTOP, Side::Right);
        assert_eq!(barriers.len(), 1);
        assert_eq!(barriers[0].line, (3840, 0, 3840, 1079));
    }

    #[test]
    fn no_monitors_no_barriers() {
        assert!(outer_barriers(&[], Side::Left).is_empty());
    }

    #[test]
    fn return_point_is_just_inside_the_edge() {
        let m = rect(0, 1080, 1280, 720);
        assert_eq!(return_point(Side::Bottom, &m, 0.0), (0.0, 1798.0));
        assert_eq!(return_point(Side::Bottom, &m, 1.0), (1279.0, 1798.0));
        assert_eq!(return_point(Side::Left, &m, 0.5), (1.0, 1080.0 + 359.5));
        assert_eq!(return_point(Side::Right, &m, 0.0), (1278.0, 1080.0));
        assert_eq!(return_point(Side::Top, &m, 0.0), (0.0, 1081.0));
    }

    #[test]
    fn return_point_clamps_along() {
        let m = rect(0, 0, 100, 100);
        assert_eq!(
            return_point(Side::Bottom, &m, -3.0),
            return_point(Side::Bottom, &m, 0.0)
        );
        assert_eq!(
            return_point(Side::Bottom, &m, 7.0),
            return_point(Side::Bottom, &m, 1.0)
        );
    }

    #[test]
    fn opposite_and_wire_names() {
        assert_eq!(Side::Bottom.opposite(), Side::Top);
        assert_eq!(Side::Left.opposite(), Side::Right);
        assert_eq!(serde_json::to_string(&Side::Top).unwrap(), "\"top\"");
        for name in ["left", "right", "top", "bottom"] {
            let side: Side = serde_json::from_str(&format!("\"{name}\"")).unwrap();
            assert_eq!(serde_json::to_string(&side).unwrap(), format!("\"{name}\""));
        }
    }

    #[test]
    fn split_measures_push_outwards() {
        assert_eq!(Side::Bottom.split(3.0, 5.0), (5.0, 3.0));
        assert_eq!(Side::Top.split(3.0, 5.0), (-5.0, 3.0));
        assert_eq!(Side::Left.split(-2.0, 1.0), (2.0, 1.0));
    }

    #[test]
    fn touch_is_the_outer_pixel_row_of_a_watched_monitor() {
        let barriers = outer_barriers(&DESKTOP, Side::Bottom);
        assert!(touched_barrier(&barriers, Side::Bottom, (100, 1799)).is_some());
        assert!(
            touched_barrier(&barriers, Side::Bottom, (100, 1800)).is_some(),
            "beyond the edge counts"
        );
        assert!(touched_barrier(&barriers, Side::Bottom, (100, 1798)).is_none());
        // Bottom row of a monitor that isn't the lowest one.
        assert!(touched_barrier(&barriers, Side::Bottom, (2000, 1079)).is_none());
        // Past the watched monitor's width.
        assert!(touched_barrier(&barriers, Side::Bottom, (1280, 1799)).is_none());
    }

    #[test]
    fn touch_on_left_and_right_edges() {
        let left = outer_barriers(&DESKTOP, Side::Left);
        assert_eq!(
            touched_barrier(&left, Side::Left, (0, 1500)).map(|b| b.monitor),
            Some(DESKTOP[2])
        );
        let right = outer_barriers(&DESKTOP, Side::Right);
        assert!(touched_barrier(&right, Side::Right, (3839, 10)).is_some());
        assert!(touched_barrier(&right, Side::Right, (3838, 10)).is_none());
    }
}
