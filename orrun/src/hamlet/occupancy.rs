//! Pixel occupancy grid so non-aligned houses cannot overlap.

use glam::Vec2;

#[derive(Clone, Debug)]
pub struct Occupancy {
    cell: f32,
    origin: Vec2,
    width: usize,
    height: usize,
    cells: Vec<u8>,
}

impl Occupancy {
    pub fn setup(world_half: f32, cell_size: f32) -> Self {
        let cell = cell_size.max(0.1);
        let span = world_half * 2.0;
        let width = (span / cell).ceil() as usize + 2;
        let height = width;
        let origin = Vec2::new(-world_half - cell, -world_half - cell);
        Self {
            cell,
            origin,
            width,
            height,
            cells: vec![0; width * height],
        }
    }

    fn index(&self, ix: isize, iy: isize) -> Option<usize> {
        if ix < 0 || iy < 0 || ix as usize >= self.width || iy as usize >= self.height {
            return None;
        }
        Some(iy as usize * self.width + ix as usize)
    }

    pub fn world_to_cell(&self, p: Vec2) -> (isize, isize) {
        (
            ((p.x - self.origin.x) / self.cell).floor() as isize,
            ((p.y - self.origin.y) / self.cell).floor() as isize,
        )
    }

    pub fn cell_center(&self, ix: isize, iy: isize) -> Vec2 {
        self.origin
            + Vec2::new(
                (ix as f32 + 0.5) * self.cell,
                (iy as f32 + 0.5) * self.cell,
            )
    }

    pub fn fits_obb(
        &self,
        center: Vec2,
        half_x: f32,
        half_z: f32,
        yaw: f32,
        inflate: f32,
    ) -> bool {
        !self.obb_hits_occupied(center, half_x, half_z, yaw, inflate)
    }

    pub fn stamp_obb(&mut self, center: Vec2, half_x: f32, half_z: f32, yaw: f32, inflate: f32) {
        let hx = half_x + inflate;
        let hz = half_z + inflate;
        let x_axis = Vec2::new(yaw.cos(), -yaw.sin());
        let z_axis = Vec2::new(yaw.sin(), yaw.cos());
        let corners = [
            center + x_axis * hx + z_axis * hz,
            center - x_axis * hx + z_axis * hz,
            center - x_axis * hx - z_axis * hz,
            center + x_axis * hx - z_axis * hz,
        ];
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for c in corners {
            min_x = min_x.min(c.x);
            min_y = min_y.min(c.y);
            max_x = max_x.max(c.x);
            max_y = max_y.max(c.y);
        }
        let (min_cx, min_cy) = self.world_to_cell(Vec2::new(min_x, min_y));
        let (max_cx, max_cy) = self.world_to_cell(Vec2::new(max_x, max_y));
        for iy in min_cy..=max_cy {
            for ix in min_cx..=max_cx {
                let Some(idx) = self.index(ix, iy) else {
                    continue;
                };
                let p = self.cell_center(ix, iy);
                let local = p - center;
                if local.dot(x_axis).abs() <= hx && local.dot(z_axis).abs() <= hz {
                    self.cells[idx] = 1;
                }
            }
        }
    }

    pub fn stamp_polygon(&mut self, poly: &[Vec2]) {
        assert!(
            poly.len() >= 3,
            "HamletLabPlanner: market polygon needs >= 3 verts"
        );
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for p in poly {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
        let (min_cx, min_cy) = self.world_to_cell(Vec2::new(min_x, min_y));
        let (max_cx, max_cy) = self.world_to_cell(Vec2::new(max_x, max_y));
        for iy in min_cy..=max_cy {
            for ix in min_cx..=max_cx {
                let Some(idx) = self.index(ix, iy) else {
                    continue;
                };
                if point_in_polygon(self.cell_center(ix, iy), poly) {
                    self.cells[idx] = 1;
                }
            }
        }
    }

    /// Returns true if the OBB hits occupied cells or leaves the grid.
    fn obb_hits_occupied(
        &self,
        center: Vec2,
        half_x: f32,
        half_z: f32,
        yaw: f32,
        inflate: f32,
    ) -> bool {
        self.visit_obb(center, half_x, half_z, yaw, inflate, |cells, idx| {
            cells[idx] != 0
        })
    }

    /// Visits cell centres inside the OBB. Callback returning true aborts early.
    /// Out-of-bounds cells count as a hit (returns true).
    fn visit_obb(
        &self,
        center: Vec2,
        half_x: f32,
        half_z: f32,
        yaw: f32,
        inflate: f32,
        mut on_cell: impl FnMut(&[u8], usize) -> bool,
    ) -> bool {
        let hx = half_x + inflate;
        let hz = half_z + inflate;
        let x_axis = Vec2::new(yaw.cos(), -yaw.sin());
        let z_axis = Vec2::new(yaw.sin(), yaw.cos());
        let corners = [
            center + x_axis * hx + z_axis * hz,
            center - x_axis * hx + z_axis * hz,
            center - x_axis * hx - z_axis * hz,
            center + x_axis * hx - z_axis * hz,
        ];
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for c in corners {
            min_x = min_x.min(c.x);
            min_y = min_y.min(c.y);
            max_x = max_x.max(c.x);
            max_y = max_y.max(c.y);
        }
        let (min_cx, min_cy) = self.world_to_cell(Vec2::new(min_x, min_y));
        let (max_cx, max_cy) = self.world_to_cell(Vec2::new(max_x, max_y));
        for iy in min_cy..=max_cy {
            for ix in min_cx..=max_cx {
                let Some(idx) = self.index(ix, iy) else {
                    return true;
                };
                let p = self.cell_center(ix, iy);
                let local = p - center;
                let lx = local.dot(x_axis);
                let lz = local.dot(z_axis);
                if lx.abs() <= hx && lz.abs() <= hz && on_cell(&self.cells, idx) {
                    return true;
                }
            }
        }
        false
    }

    pub fn occupied_dots(&self, stride: usize) -> Vec<Vec2> {
        let stride = stride.max(1);
        let mut out = Vec::new();
        for iy in (0..self.height).step_by(stride) {
            for ix in (0..self.width).step_by(stride) {
                if self.cells[iy * self.width + ix] != 0 {
                    out.push(self.cell_center(ix as isize, iy as isize));
                }
            }
        }
        out
    }
}

pub fn point_in_polygon(p: Vec2, poly: &[Vec2]) -> bool {
    let mut inside = false;
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let pi = poly[i];
        let pj = poly[j];
        if (pi.y > p.y) != (pj.y > p.y) {
            let x_cross = (pj.x - pi.x) * (p.y - pi.y) / (pj.y - pi.y) + pi.x;
            if p.x < x_cross {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_and_reject_overlap() {
        let mut occ = Occupancy::setup(20.0, 0.4);
        occ.stamp_obb(Vec2::ZERO, 2.0, 3.0, 0.0, 0.0);
        assert!(!occ.fits_obb(Vec2::ZERO, 2.0, 3.0, 0.0, 0.0));
        assert!(occ.fits_obb(Vec2::new(10.0, 0.0), 2.0, 3.0, 0.0, 0.0));
    }

    #[test]
    fn polygon_stamp_blocks_interior() {
        let mut occ = Occupancy::setup(20.0, 0.35);
        let poly = [
            Vec2::new(-4.0, -4.0),
            Vec2::new(4.0, -4.0),
            Vec2::new(4.0, 4.0),
            Vec2::new(-4.0, 4.0),
        ];
        occ.stamp_polygon(&poly);
        assert!(!occ.fits_obb(Vec2::ZERO, 1.0, 1.0, 0.0, 0.0));
        assert!(occ.fits_obb(Vec2::new(12.0, 0.0), 1.0, 1.0, 0.0, 0.0));
    }
}
