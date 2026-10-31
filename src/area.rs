//! The area to generate: a square or a circle of chunks around a centre
//! chunk, with the same meaning as Horizon's `/pregen` (`horizon_client::pregen`).
//! The area is swept row by row (north to south); each row's chunks form one
//! contiguous span of columns, which keeps the pipeline's bookkeeping simple.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Square,
    Circle,
}

#[derive(Clone, Copy, Debug)]
pub struct Area {
    /// The centre chunk.
    pub center: (i32, i32),
    /// The radius in chunks.
    pub radius: i32,
    pub shape: Shape,
}

impl Area {
    pub fn new(center: (i32, i32), radius: i32, shape: Shape) -> Self {
        Self {
            center,
            radius: radius.max(0),
            shape,
        }
    }

    /// How far from the centre column the area reaches at `d` rows from
    /// the centre row, if it reaches that row at all.
    pub fn half_width(&self, d: i32) -> Option<i32> {
        let (r, d) = (i64::from(self.radius), i64::from(d));
        match self.shape {
            Shape::Square => (d.abs() <= r).then_some(self.radius),
            Shape::Circle => (d * d <= r * r).then(|| (r * r - d * d).isqrt() as i32),
        }
    }

    #[allow(dead_code)]
    pub fn contains(&self, x: i32, z: i32) -> bool {
        self.half_width(z - self.center.1)
            .is_some_and(|h| (x - self.center.0).abs() <= h)
    }

    /// The area's columns in row `z`, inclusive.
    pub fn span(&self, z: i32) -> Option<(i32, i32)> {
        let h = self.half_width(z - self.center.1)?;
        Some((self.center.0 - h, self.center.0 + h))
    }

    /// The columns of row `z` within `margin` chunks (Chebyshev distance) of
    /// an area chunk, inclusive: the union of the neighbouring rows' spans,
    /// each widened by `margin`. The area is convex and symmetric about its
    /// centre column, so the union is the widest of them.
    pub fn span_within(&self, z: i32, margin: i32) -> Option<(i32, i32)> {
        let mut best: Option<i32> = None;
        for dz in -margin..=margin {
            if let Some(h) = self.half_width(z + dz - self.center.1) {
                best = Some(best.map_or(h, |b| b.max(h)));
            }
        }
        best.map(|h| (self.center.0 - h - margin, self.center.0 + h + margin))
    }

    /// The first and last rows holding area chunks.
    pub fn rows(&self) -> (i32, i32) {
        (self.center.1 - self.radius, self.center.1 + self.radius)
    }

    /// The columns the widest row covers, inclusive.
    pub fn columns(&self) -> (i32, i32) {
        (self.center.0 - self.radius, self.center.0 + self.radius)
    }

    /// How many chunks the area holds.
    pub fn total(&self) -> u64 {
        let (z0, z1) = self.rows();
        (z0..=z1)
            .filter_map(|z| self.span(z))
            .map(|(lo, hi)| (i64::from(hi) - i64::from(lo) + 1) as u64)
            .sum()
    }
}

/// How wide a memory-bounded strip is, in chunks (Horizon's pregen strips).
pub const STRIP_WIDTH: i32 = 256;

/// Column bands of at most `width` chunks, west to east, covering `lo..=hi`.
/// `width` below 1 is treated as one column.
pub fn strips(lo: i32, hi: i32, width: i32) -> Vec<(i32, i32)> {
    let width = width.max(1);
    let mut out = Vec::new();
    if hi < lo {
        return out;
    }
    let mut x = lo;
    while x <= hi {
        let end = x.saturating_add(width - 1).min(hi);
        out.push((x, end));
        if end == i32::MAX {
            break;
        }
        x = end + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_cover_exactly_the_area() {
        for shape in [Shape::Square, Shape::Circle] {
            for (center, radius) in [((0, 0), 0), ((3, -7), 5), ((-40, 12), 130), ((0, 0), 300)] {
                let area = Area::new(center, radius, shape);
                let r = i64::from(radius);
                let expected = (-radius..=radius)
                    .flat_map(|dz| (-radius..=radius).map(move |dx| (dx, dz)))
                    .filter(|&(dx, dz)| {
                        shape == Shape::Square
                            || i64::from(dx).pow(2) + i64::from(dz).pow(2) <= r * r
                    })
                    .count() as u64;
                assert_eq!(area.total(), expected, "{shape:?} {radius}");
                let (z0, z1) = area.rows();
                let mut seen = 0u64;
                for z in z0 - 2..=z1 + 2 {
                    match area.span(z) {
                        Some((lo, hi)) => {
                            assert!(area.contains(lo, z) && area.contains(hi, z));
                            assert!(!area.contains(lo - 1, z) && !area.contains(hi + 1, z));
                            seen += u64::try_from(hi - lo + 1).unwrap();
                        }
                        None => assert!(!(z0..=z1).contains(&z) || shape == Shape::Circle),
                    }
                }
                assert_eq!(seen, expected, "{shape:?} {radius}");
            }
        }
    }

    #[test]
    fn margins_reach_every_neighbour_of_the_area() {
        for shape in [Shape::Square, Shape::Circle] {
            let area = Area::new((5, -3), 9, shape);
            for margin in 1..=2 {
                let (z0, z1) = area.rows();
                for z in z0 - margin - 1..=z1 + margin + 1 {
                    let within = area.span_within(z, margin);
                    for x in area.center.0 - 15..=area.center.0 + 15 {
                        let near = (-margin..=margin)
                            .any(|dz| (-margin..=margin).any(|dx| area.contains(x + dx, z + dz)));
                        let inside = within.is_some_and(|(lo, hi)| (lo..=hi).contains(&x));
                        assert_eq!(inside, near, "{shape:?} margin {margin} at {x} {z}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_huge_area_is_planned_at_once() {
        let started = std::time::Instant::now();
        let area = Area::new((0, 0), 30_000_000 >> 4, Shape::Circle);
        assert!(area.total() > 11_000_000_000_000);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn strips_are_256_columns_covering_the_range() {
        assert_eq!(strips(0, 10, STRIP_WIDTH), vec![(0, 10)]);
        assert_eq!(strips(0, 255, STRIP_WIDTH), vec![(0, 255)]);
        assert_eq!(strips(0, 256, STRIP_WIDTH), vec![(0, 255), (256, 256)]);
        assert_eq!(strips(0, 10, 4), vec![(0, 3), (4, 7), (8, 10)]);
        let wide = strips(-10, 500, STRIP_WIDTH);
        assert_eq!(wide, vec![(-10, 245), (246, 500)]);
        let area = Area::new((0, 0), 300, Shape::Square);
        let (lo, hi) = area.columns();
        let bands = strips(lo, hi, STRIP_WIDTH);
        assert_eq!(bands.first().unwrap().0, lo);
        assert_eq!(bands.last().unwrap().1, hi);
        assert!(bands.iter().all(|&(a, b)| b - a + 1 <= STRIP_WIDTH));
    }
}
