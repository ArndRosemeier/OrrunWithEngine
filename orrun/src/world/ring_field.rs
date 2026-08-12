//! Spatial indices over hydro polylines and rings.
//!
//! Coast and lake outlines are authored densely — a continental coast is tens of
//! thousands of vertices — and every surface column asks how far it is from
//! them. Walking the whole ring per column is what forced the earlier decimated
//! "query ring", which in turn made the 2D overlay and the 3D shoreline two
//! different curves. Indexing the *authored* geometry removes both problems:
//! queries touch a handful of nearby segments and there is only one coastline.
//!
//! A [`SegmentField`] buckets segments into a uniform grid. A [`RingField`] adds
//! a cell-centre inside/outside bitmap, so a signed distance costs one bitmap
//! lookup, a short local crossing count, and a small nearest-segment search.

use glam::Vec2;

/// Segments per grid cell to aim for; trades memory against search width.
const SEGMENTS_PER_CELL: usize = 16;
const MIN_CELL_M: f32 = 24.0;
const MAX_CELL_M: f32 = 2_000.0;
const MAX_CELLS: usize = 4_000_000;
/// Grid cells per axis behind one coarse occupancy cell.
const COARSE_STEP: usize = 8;

/// Closest point on a polyline.
#[derive(Clone, Copy, Debug)]
pub struct SegmentHit {
    /// Index of the segment's first point.
    pub segment: usize,
    /// Parameter along that segment, `0..=1`.
    pub t: f32,
    pub distance: f32,
}

/// Uniform grid over the segments of one or more polylines, or a closed ring.
#[derive(Debug)]
pub struct SegmentField {
    points: Vec<Vec2>,
    /// The point pair each segment runs between.
    ///
    /// Explicit rather than implied by "index and index + 1", so several
    /// separate polylines can share one grid: a brook network is a few hundred
    /// short traces, and indexing each on its own grid would mean asking every
    /// one of them how far away it is.
    ends: Vec<[u32; 2]>,
    min: Vec2,
    max: Vec2,
    origin: Vec2,
    cell_m: f32,
    nx: usize,
    nz: usize,
    /// CSR offsets into `items`, length `nx * nz + 1`.
    starts: Vec<u32>,
    items: Vec<u32>,
    /// "Any segment in this block of cells", so a query far from the geometry
    /// can be rejected without walking rings of empty cells.
    coarse: Vec<bool>,
    coarse_nx: usize,
}

impl SegmentField {
    /// `None` when there are too few points to form a segment.
    pub fn build(points: &[Vec2], closed: bool) -> Option<Self> {
        let needed = if closed { 3 } else { 2 };
        if points.len() < needed {
            return None;
        }
        let n = points.len() as u32;
        let last = if closed { n } else { n - 1 };
        let ends = (0..last).map(|i| [i, (i + 1) % n]).collect();
        Self::from_parts(points.to_vec(), ends)
    }

    /// One grid over several open polylines, indexed end to end.
    ///
    /// Segment indices run through the paths in order; [`Self::segment_ends`]
    /// gives back the point indices, which is how a caller finds which path a
    /// hit belongs to.
    pub fn build_paths(paths: &[Vec<Vec2>]) -> Option<Self> {
        let mut points = Vec::new();
        let mut ends = Vec::new();
        for path in paths {
            if path.len() < 2 {
                continue;
            }
            let base = points.len() as u32;
            points.extend_from_slice(path);
            ends.extend((0..path.len() as u32 - 1).map(|i| [base + i, base + i + 1]));
        }
        Self::from_parts(points, ends)
    }

    fn from_parts(points: Vec<Vec2>, ends: Vec<[u32; 2]>) -> Option<Self> {
        if ends.is_empty() || points.iter().any(|p| !p.is_finite()) {
            return None;
        }
        let (min, max) = aabb(&points);
        let segments = ends.len();
        let cell_m = choose_cell(min, max, segments);
        // One cell of padding keeps every segment away from the border, so a
        // query inside the grid can always compare against the cell it lands in.
        let origin = min - Vec2::splat(cell_m);
        let nx = (((max.x - min.x) / cell_m).ceil() as usize) + 3;
        let nz = (((max.y - min.y) / cell_m).ceil() as usize) + 3;

        let span_of = |seg: usize| {
            let (a, b) = (points[ends[seg][0] as usize], points[ends[seg][1] as usize]);
            (
                cell_of(a.min(b), origin, cell_m, nx, nz),
                cell_of(a.max(b), origin, cell_m, nx, nz),
            )
        };

        let mut starts = vec![0u32; nx * nz + 1];
        for seg in 0..segments {
            let (lo, hi) = span_of(seg);
            for cz in lo.1..=hi.1 {
                for cx in lo.0..=hi.0 {
                    starts[cz * nx + cx + 1] += 1;
                }
            }
        }
        for i in 0..nx * nz {
            starts[i + 1] += starts[i];
        }
        let mut items = vec![0u32; starts[nx * nz] as usize];
        let mut cursor = starts.clone();
        for seg in 0..segments {
            let (lo, hi) = span_of(seg);
            for cz in lo.1..=hi.1 {
                for cx in lo.0..=hi.0 {
                    let cell = cz * nx + cx;
                    items[cursor[cell] as usize] = seg as u32;
                    cursor[cell] += 1;
                }
            }
        }

        let coarse_nx = nx.div_ceil(COARSE_STEP);
        let coarse_nz = nz.div_ceil(COARSE_STEP);
        let mut coarse = vec![false; coarse_nx * coarse_nz];
        for cz in 0..nz {
            for cx in 0..nx {
                let i = cz * nx + cx;
                if starts[i + 1] > starts[i] {
                    coarse[(cz / COARSE_STEP) * coarse_nx + cx / COARSE_STEP] = true;
                }
            }
        }

        Some(Self {
            points,
            ends,
            min,
            max,
            origin,
            cell_m,
            nx,
            nz,
            starts,
            items,
            coarse,
            coarse_nx,
        })
    }

    /// The two points segment `seg` runs between.
    #[inline]
    pub fn segment(&self, seg: usize) -> (Vec2, Vec2) {
        let [a, b] = self.ends[seg];
        (self.points[a as usize], self.points[b as usize])
    }

    /// Point indices of segment `seg`, for callers carrying per-point data.
    #[inline]
    pub fn segment_ends(&self, seg: usize) -> (usize, usize) {
        let [a, b] = self.ends[seg];
        (a as usize, b as usize)
    }

    /// Distance from `p` to the geometry's bounding box; `0` when inside it.
    pub fn aabb_distance(&self, p: Vec2) -> f32 {
        let dx = (self.min.x - p.x).max(p.x - self.max.x).max(0.0);
        let dz = (self.min.y - p.y).max(p.y - self.max.y).max(0.0);
        dx.hypot(dz)
    }

    fn cell_edges(&self, cx: usize, cz: usize) -> &[u32] {
        let i = cz * self.nx + cx;
        let (a, b) = (self.starts[i] as usize, self.starts[i + 1] as usize);
        &self.items[a..b]
    }

    /// Nearest point on the geometry within `max_dist`, else `None`.
    ///
    /// The bound is required, not a convenience: without it a query far from the
    /// geometry has to walk every cell between itself and the nearest segment,
    /// which is most of the grid for an inland column asking about the coast.
    pub fn nearest_within(&self, p: Vec2, max_dist: f32) -> Option<SegmentHit> {
        if !self.any_within(p, max_dist) {
            return None;
        }
        // Queries may sit outside the grid; the search then starts from the
        // nearest border cell and has to reach `max_dist` measured from `p`.
        let (cx, cz) = self.cell_index(p);
        let reach = max_dist + p.distance(self.cell_centre(cx, cz));
        let mut best: Option<SegmentHit> = None;
        let max_radius = ((reach / self.cell_m).ceil() as usize + 1).min(self.nx.max(self.nz));
        for radius in 0..=max_radius {
            // Everything within `covered` of `p` has already been searched.
            if let Some(hit) = best {
                if hit.distance <= self.covered_margin(p, cx, cz, radius) {
                    break;
                }
            }
            let mut touched = false;
            for (gx, gz) in self.ring_cells(cx, cz, radius) {
                touched = true;
                for &seg in self.cell_edges(gx, gz) {
                    let (a, b) = self.segment(seg as usize);
                    let (t, distance) = point_segment_dist(p, a, b);
                    if best.map(|h| distance < h.distance).unwrap_or(true) {
                        best = Some(SegmentHit {
                            segment: seg as usize,
                            t,
                            distance,
                        });
                    }
                }
            }
            if !touched && best.is_some() {
                break;
            }
        }
        best.filter(|hit| hit.distance <= max_dist)
    }

    /// Is any segment inside the box of radius `max_dist` around `p`?
    fn any_within(&self, p: Vec2, max_dist: f32) -> bool {
        if self.aabb_distance(p) > max_dist {
            return false;
        }
        let lo = self.coarse_of(p - Vec2::splat(max_dist));
        let hi = self.coarse_of(p + Vec2::splat(max_dist));
        for bz in lo.1..=hi.1 {
            for bx in lo.0..=hi.0 {
                if self.coarse[bz * self.coarse_nx + bx] {
                    return true;
                }
            }
        }
        false
    }

    fn coarse_of(&self, p: Vec2) -> (usize, usize) {
        let (cx, cz) = cell_of(p, self.origin, self.cell_m, self.nx, self.nz);
        (cx / COARSE_STEP, cz / COARSE_STEP)
    }

    fn inside_grid(&self, p: Vec2) -> bool {
        p.x >= self.origin.x
            && p.y >= self.origin.y
            && p.x < self.origin.x + self.nx as f32 * self.cell_m
            && p.y < self.origin.y + self.nz as f32 * self.cell_m
    }

    fn cell_index(&self, p: Vec2) -> (usize, usize) {
        cell_of(p, self.origin, self.cell_m, self.nx, self.nz)
    }

    fn cell_centre(&self, cx: usize, cz: usize) -> Vec2 {
        self.origin
            + Vec2::new(
                (cx as f32 + 0.5) * self.cell_m,
                (cz as f32 + 0.5) * self.cell_m,
            )
    }

    /// Radius around `p` fully covered by the cells searched so far.
    fn covered_margin(&self, p: Vec2, cx: usize, cz: usize, radius: usize) -> f32 {
        if radius == 0 {
            return 0.0;
        }
        let r = (radius - 1) as i64;
        let lo_x = self.origin.x + (cx as i64 - r) as f32 * self.cell_m;
        let lo_z = self.origin.y + (cz as i64 - r) as f32 * self.cell_m;
        let hi_x = self.origin.x + (cx as i64 + r + 1) as f32 * self.cell_m;
        let hi_z = self.origin.y + (cz as i64 + r + 1) as f32 * self.cell_m;
        (p.x - lo_x)
            .min(hi_x - p.x)
            .min(p.y - lo_z)
            .min(hi_z - p.y)
            .max(0.0)
    }

    /// Cells at Chebyshev distance `radius`, clipped to the grid.
    fn ring_cells(&self, cx: usize, cz: usize, radius: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let r = radius as i64;
        let (cx, cz) = (cx as i64, cz as i64);
        let push = |x: i64, z: i64, out: &mut Vec<(usize, usize)>| {
            if x >= 0 && z >= 0 && (x as usize) < self.nx && (z as usize) < self.nz {
                out.push((x as usize, z as usize));
            }
        };
        if radius == 0 {
            push(cx, cz, &mut out);
            return out;
        }
        for x in (cx - r)..=(cx + r) {
            push(x, cz - r, &mut out);
            push(x, cz + r, &mut out);
        }
        for z in (cz - r + 1)..=(cz + r - 1) {
            push(cx - r, z, &mut out);
            push(cx + r, z, &mut out);
        }
        out
    }
}

/// A closed ring with a signed distance: **positive inside**.
#[derive(Debug)]
pub struct RingField {
    field: SegmentField,
    /// Insideness sampled at each cell centre.
    inside: Vec<bool>,
}

impl RingField {
    pub fn build(ring: &[Vec2]) -> Option<Self> {
        let field = SegmentField::build(ring, true)?;
        let inside = rasterize_inside(&field);
        Some(Self { field, inside })
    }

    /// Signed distance to the ring; positive inside, negative outside.
    ///
    /// **Saturating**: beyond `max_dist` the magnitude is reported as `max_dist`
    /// with the correct sign. Shore shaping only reads the first few hundred
    /// metres, and an exact figure far away would cost a grid-wide search.
    pub fn signed_distance(&self, p: Vec2, max_dist: f32) -> f32 {
        let inside = self.contains(p);
        let distance = match self.field.nearest_within(p, max_dist) {
            Some(hit) => hit.distance,
            None => max_dist,
        };
        if inside {
            distance
        } else {
            -distance
        }
    }

    /// Point-in-ring test.
    ///
    /// The bitmap answers for the cell centre; only crossings between `p` and
    /// that centre can change it, and both points lie in the same convex cell,
    /// so the correction needs just that cell's segments.
    pub fn contains(&self, p: Vec2) -> bool {
        if !self.field.inside_grid(p) {
            return false;
        }
        let (cx, cz) = self.field.cell_index(p);
        let centre = self.field.cell_centre(cx, cz);
        let mut inside = self.inside[cz * self.field.nx + cx];
        for &seg in self.field.cell_edges(cx, cz) {
            let (a, b) = self.field.segment(seg as usize);
            if segments_cross(p, centre, a, b) {
                inside = !inside;
            }
        }
        inside
    }
}

/// Scanline fill of the ring at cell-centre resolution.
fn rasterize_inside(field: &SegmentField) -> Vec<bool> {
    let mut inside = vec![false; field.nx * field.nz];

    // Bucket segments by the rows they span so each row only sees its own.
    let mut rows: Vec<Vec<u32>> = vec![Vec::new(); field.nz];
    for seg in 0..field.ends.len() {
        let (a, b) = field.segment(seg);
        let lo = row_of(a.y.min(b.y), field);
        let hi = row_of(a.y.max(b.y), field);
        for row in rows.iter_mut().take(hi + 1).skip(lo) {
            row.push(seg as u32);
        }
    }

    let mut crossings: Vec<f32> = Vec::new();
    for cz in 0..field.nz {
        let y = field.origin.y + (cz as f32 + 0.5) * field.cell_m;
        crossings.clear();
        for &seg in &rows[cz] {
            let (a, b) = field.segment(seg as usize);
            if (a.y > y) != (b.y > y) {
                let t = (y - a.y) / (b.y - a.y);
                crossings.push(a.x + (b.x - a.x) * t);
            }
        }
        if crossings.is_empty() {
            continue;
        }
        crossings.sort_by(|l, r| l.partial_cmp(r).expect("finite ring crossings"));
        // March left to right, flipping parity at each crossing.
        let mut next = 0usize;
        let mut parity = false;
        for cx in 0..field.nx {
            let x = field.origin.x + (cx as f32 + 0.5) * field.cell_m;
            while next < crossings.len() && crossings[next] <= x {
                parity = !parity;
                next += 1;
            }
            inside[cz * field.nx + cx] = parity;
        }
    }
    inside
}

fn row_of(y: f32, field: &SegmentField) -> usize {
    let r = ((y - field.origin.y) / field.cell_m).floor();
    (r.max(0.0) as usize).min(field.nz - 1)
}

fn choose_cell(min: Vec2, max: Vec2, segments: usize) -> f32 {
    let w = (max.x - min.x).max(1.0);
    let h = (max.y - min.y).max(1.0);
    let target = (segments * SEGMENTS_PER_CELL).max(16) as f32;
    let mut cell = ((w * h) / target).sqrt().clamp(MIN_CELL_M, MAX_CELL_M);
    // Never let a wide, sparse outline blow up the grid.
    while ((w / cell).ceil() as usize + 3) * ((h / cell).ceil() as usize + 3) > MAX_CELLS {
        cell *= 2.0;
    }
    cell
}

fn aabb(points: &[Vec2]) -> (Vec2, Vec2) {
    let mut min = points[0];
    let mut max = points[0];
    for p in points.iter().skip(1) {
        min = min.min(*p);
        max = max.max(*p);
    }
    (min, max)
}

fn cell_of(p: Vec2, origin: Vec2, cell_m: f32, nx: usize, nz: usize) -> (usize, usize) {
    let x = ((p.x - origin.x) / cell_m).floor().max(0.0) as usize;
    let z = ((p.y - origin.y) / cell_m).floor().max(0.0) as usize;
    (x.min(nx - 1), z.min(nz - 1))
}

/// Distance from `p` to segment `a..b`, with the parameter of the closest point.
pub fn point_segment_dist(p: Vec2, a: Vec2, b: Vec2) -> (f32, f32) {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 < 1e-6 {
        return (0.0, p.distance(a));
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    (t, p.distance(a + ab * t))
}

/// Proper crossing test; touching endpoints do not count.
fn segments_cross(p0: Vec2, p1: Vec2, q0: Vec2, q1: Vec2) -> bool {
    let d1 = cross(q1 - q0, p0 - q0);
    let d2 = cross(q1 - q0, p1 - q0);
    let d3 = cross(p1 - p0, q0 - p0);
    let d4 = cross(p1 - p0, q1 - p0);
    ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0))
}

#[inline]
fn cross(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circle(radius: f32, n: usize, centre: Vec2) -> Vec<Vec2> {
        (0..n)
            .map(|k| {
                let a = std::f32::consts::TAU * k as f32 / n as f32;
                centre + Vec2::new(radius * a.cos(), radius * a.sin())
            })
            .collect()
    }

    /// Brute-force reference the index must match.
    fn reference_signed(ring: &[Vec2], p: Vec2) -> f32 {
        let n = ring.len();
        let mut d = f32::INFINITY;
        let mut inside = false;
        for i in 0..n {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            d = d.min(point_segment_dist(p, a, b).1);
            if (a.y > p.y) != (b.y > p.y) {
                let x = (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x;
                if p.x < x {
                    inside = !inside;
                }
            }
        }
        if inside {
            d
        } else {
            -d
        }
    }

    const RANGE: f32 = 2_400.0;

    #[test]
    fn the_index_matches_a_brute_force_ring_walk() {
        let ring = circle(3_000.0, 2_048, Vec2::new(9_000.0, 5_000.0));
        let field = RingField::build(&ring).expect("ring field");
        let mut checked = 0;
        for iz in 0..40 {
            for ix in 0..40 {
                let p = Vec2::new(4_000.0 + ix as f32 * 250.0, iz as f32 * 250.0);
                let want = reference_signed(&ring, p);
                let got = field.signed_distance(p, RANGE);
                assert_eq!(
                    got > 0.0,
                    want > 0.0,
                    "at {p:?} the index disagrees about inside/outside"
                );
                if want.abs() <= RANGE {
                    assert!(
                        (got - want).abs() < 1.0,
                        "at {p:?}: index {got} vs brute force {want}"
                    );
                } else {
                    assert!(
                        got.abs() <= RANGE + 1.0,
                        "beyond the query range the magnitude must saturate, got {got}"
                    );
                }
                checked += 1;
            }
        }
        assert!(checked > 500, "expected a dense comparison, got {checked}");
    }

    #[test]
    fn separate_paths_in_one_grid_do_not_join_up() {
        // The whole reason for `build_paths`: a naive concatenation would leave
        // a segment bridging the gap between two brooks, and a query in that gap
        // would be told it is standing in water.
        let left = vec![Vec2::new(0.0, 0.0), Vec2::new(0.0, 100.0)];
        let right = vec![Vec2::new(400.0, 0.0), Vec2::new(400.0, 100.0)];
        let field = SegmentField::build_paths(&[left, right]).expect("two paths");
        let middle = Vec2::new(200.0, 50.0);
        let hit = field
            .nearest_within(middle, 300.0)
            .expect("both are in range");
        assert!(
            (hit.distance - 200.0).abs() < 0.01,
            "the gap must measure to the nearer path, got {}",
            hit.distance
        );
        assert!(field.nearest_within(middle, 150.0).is_none());

        // Point indices come back so a caller can find which path was hit.
        let hit = field
            .nearest_within(Vec2::new(402.0, 50.0), 10.0)
            .expect("right path");
        let (a, b) = field.segment_ends(hit.segment);
        assert_eq!((a, b), (2, 3));
    }

    #[test]
    fn a_concave_ring_keeps_its_pocket_outside() {
        // A C shape: the pocket is outside even though it is inside the AABB.
        let mut ring = Vec::new();
        for k in 0..=180 {
            let a = std::f32::consts::PI * (k as f32 / 180.0) + std::f32::consts::FRAC_PI_2;
            ring.push(Vec2::new(1_000.0 * a.cos(), 1_000.0 * a.sin()));
        }
        for k in (0..=180).rev() {
            let a = std::f32::consts::PI * (k as f32 / 180.0) + std::f32::consts::FRAC_PI_2;
            ring.push(Vec2::new(700.0 * a.cos(), 700.0 * a.sin()));
        }
        let field = RingField::build(&ring).expect("ring field");
        assert!(field.contains(Vec2::new(-850.0, 0.0)), "inside the arm");
        assert!(
            !field.contains(Vec2::new(0.0, 0.0)),
            "the pocket is outside"
        );
        assert!(!field.contains(Vec2::new(2_000.0, 0.0)), "well outside");
    }

    #[test]
    fn far_queries_report_a_negative_distance_without_searching() {
        let ring = circle(500.0, 64, Vec2::ZERO);
        let field = RingField::build(&ring).expect("ring field");
        let sd = field.signed_distance(Vec2::new(50_000.0, 0.0), RANGE);
        assert!(sd < 0.0, "far outside must be negative, got {sd}");
    }

    #[test]
    fn an_open_polyline_finds_its_nearest_segment() {
        let line: Vec<Vec2> = (0..500).map(|k| Vec2::new(k as f32 * 20.0, 0.0)).collect();
        let field = SegmentField::build(&line, false).expect("polyline field");
        let hit = field
            .nearest_within(Vec2::new(2_000.0, 35.0), 100.0)
            .expect("hit");
        assert!((hit.distance - 35.0).abs() < 0.01, "{hit:?}");
        assert!(
            field
                .nearest_within(Vec2::new(2_000.0, 35.0), 20.0)
                .is_none(),
            "a segment beyond the bound must not be reported"
        );
    }
}
