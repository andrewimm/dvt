//! Squarified treemap layout (Bruls, Huizing, van Wijk 2000).
//!
//! Pure geometry in floating point. Callers working in terminal cells should
//! lay out in a space where rows count double (cells are ~2x taller than wide)
//! so rectangles come out visually square, then snap to cells.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Lays out `values` (all > 0, ideally sorted largest first) inside `bounds`.
/// Returns one rectangle per value, in the same order, with areas proportional
/// to the values and together tiling `bounds` exactly.
pub fn squarify(values: &[f64], bounds: FRect) -> Vec<FRect> {
    let total: f64 = values.iter().sum();
    if values.is_empty() || total <= 0.0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return vec![FRect::default(); values.len()];
    }
    let scale = bounds.w * bounds.h / total;
    let areas: Vec<f64> = values.iter().map(|v| v * scale).collect();

    let mut out = Vec::with_capacity(areas.len());
    let mut rect = bounds;
    let mut start = 0;
    while start < areas.len() {
        let side = rect.w.min(rect.h);
        let (mut sum, mut max, mut min) = (areas[start], areas[start], areas[start]);
        let mut current = worst(sum, max, min, side);
        let mut end = start + 1;
        while end < areas.len() {
            let a = areas[end];
            let next = worst(sum + a, max.max(a), min.min(a), side);
            if next > current {
                break;
            }
            (sum, max, min, current) = (sum + a, max.max(a), min.min(a), next);
            end += 1;
        }
        // The last row takes whatever space is left, absorbing rounding drift.
        let is_last = end == areas.len();
        rect = layout_row(&areas[start..end], sum, rect, is_last, &mut out);
        start = end;
    }
    out
}

/// The worst aspect ratio in a row with the given total, max and min areas,
/// laid along a side of length `side`.
fn worst(sum: f64, max: f64, min: f64, side: f64) -> f64 {
    let (s2, w2) = (sum * sum, side * side);
    (w2 * max / s2).max(s2 / (w2 * min))
}

/// Places one row along the shorter side of `rect` and returns the remainder.
fn layout_row(row: &[f64], sum: f64, rect: FRect, is_last: bool, out: &mut Vec<FRect>) -> FRect {
    if rect.w >= rect.h {
        // A column on the left edge.
        let w = if is_last {
            rect.w
        } else {
            (sum / rect.h).min(rect.w)
        };
        let mut y = rect.y;
        for (i, a) in row.iter().enumerate() {
            let h = if i == row.len() - 1 {
                rect.y + rect.h - y
            } else {
                a / w
            };
            out.push(FRect { x: rect.x, y, w, h });
            y += h;
        }
        FRect {
            x: rect.x + w,
            y: rect.y,
            w: rect.w - w,
            h: rect.h,
        }
    } else {
        // A row along the top edge.
        let h = if is_last {
            rect.h
        } else {
            (sum / rect.w).min(rect.h)
        };
        let mut x = rect.x;
        for (i, a) in row.iter().enumerate() {
            let w = if i == row.len() - 1 {
                rect.x + rect.w - x
            } else {
                a / h
            };
            out.push(FRect { x, y: rect.y, w, h });
            x += w;
        }
        FRect {
            x: rect.x,
            y: rect.y + h,
            w: rect.w,
            h: rect.h - h,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-6;

    fn check_tiling(values: &[f64], bounds: FRect) -> Vec<FRect> {
        let rects = squarify(values, bounds);
        assert_eq!(rects.len(), values.len());
        let total: f64 = values.iter().sum();
        let mut area = 0.0;
        for (r, v) in rects.iter().zip(values) {
            let expected = v / total * bounds.w * bounds.h;
            assert!(
                (r.w * r.h - expected).abs() < 1e-3 * expected.max(1.0),
                "{r:?} for {v}"
            );
            assert!(r.x >= bounds.x - EPS && r.y >= bounds.y - EPS);
            assert!(
                r.x + r.w <= bounds.x + bounds.w + EPS && r.y + r.h <= bounds.y + bounds.h + EPS
            );
            area += r.w * r.h;
        }
        assert!((area - bounds.w * bounds.h).abs() < 1e-3);
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let ox = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
                let oy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
                assert!(ox <= EPS || oy <= EPS, "{a:?} overlaps {b:?}");
            }
        }
        rects
    }

    #[test]
    fn paper_example() {
        // The worked example from the paper: 6x4 with these areas.
        let rects = check_tiling(
            &[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0],
            FRect {
                x: 0.0,
                y: 0.0,
                w: 6.0,
                h: 4.0,
            },
        );
        // First row is a 3-wide column holding the two 6s.
        assert!((rects[0].w - 3.0).abs() < EPS && (rects[1].w - 3.0).abs() < EPS);
    }

    #[test]
    fn single_and_skewed() {
        let b = FRect {
            x: 2.0,
            y: 3.0,
            w: 80.0,
            h: 48.0,
        };
        let r = check_tiling(&[5.0], b);
        assert_eq!(r[0], b);
        check_tiling(&[1000.0, 1.0, 1.0, 1.0, 0.5], b);
        let many: Vec<f64> = (1..200).rev().map(|i| i as f64).collect();
        check_tiling(&many, b);
    }

    #[test]
    fn degenerate_inputs() {
        assert!(
            squarify(
                &[],
                FRect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0
                }
            )
            .is_empty()
        );
        assert_eq!(squarify(&[1.0], FRect::default()), vec![FRect::default()]);
    }
}
