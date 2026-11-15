//! Reproject a raw [`Frame`] onto an [`OutputGrid`] through its sky WCS.
//!
//! For each output pixel `(x, y)` (1-based) the sky position comes from
//! `grid.wcs.pixel_to_radec`; the input sample position is then
//! `frame.wcs.radec_to_pixel(ra, dec)`. Output pixels outside the input
//! footprint, behind the tangent plane, or with no finite neighbour keep
//! `image = variance = NaN` and `n_contrib = 0`.
//!
//! Performance notes (task p1): the output-grid projection constants
//! (reference-point trig) are hoisted into [`OutProj`] once per call —
//! the per-pixel math is the same expression as
//! `LinearTanWcs::pixel_to_radec`, so results are bitwise identical.
//! Input sampling runs on flat slices (no per-access bounds checks) and
//! output rows are computed independently, so they are rendered in
//! parallel with rayon on large rasters. `SkyWcs::radec_to_pixel` is
//! still invoked per in-footprint pixel (its SIP fields are private, so
//! the exact inverse is not reimplementable here); pixels whose
//! SIP-free linear estimate lies more than [`margin`] outside the
//! footprint skip that call — the margin is measured per frame by
//! comparing the estimate with the exact inverse, so skipped pixels are
//! provably out of footprint and the output is unchanged.

use ndarray::Array2;
use rayon::prelude::*;

use crate::fits::{Frame, SkyWcs};

use super::error::ScienceError;
use super::grid::{LinearTanWcs, OutputGrid};
use super::image::ScienceImage;

const DEG: f64 = std::f64::consts::PI / 180.0;

/// Below this raster area rows are rendered serially; above it they are
/// spread over the rayon pool.
const PAR_MIN_PIXELS: usize = 8192;

/// Lattice resolution for the per-frame SIP-reach measurement.
const MARGIN_LATTICE: usize = 8;

/// Sampling method for [`reproject`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interp {
    /// Nearest-neighbour: exact value of the closest input pixel centre.
    /// Preserves fluxes and flags verbatim; edges stay sharp.
    Nearest,
    /// Bilinear over the four nearest input pixel centres, coefficients
    /// renormalized over the finite subset of each quantity. Variance
    /// propagates as `sum(c_i^2 * var_i)` (independent pixels); a
    /// neighbour with non-finite or non-positive variance is dropped
    /// from the variance sum, like the flux.
    #[default]
    Bilinear,
}

/// Options for [`reproject`].
#[derive(Debug, Clone, Copy)]
pub struct ReprojectOptions {
    /// Interpolation method for flux/variance/zodi. The flag bitmask is
    /// always nearest-sampled (interpolation of a bitmask is
    /// meaningless).
    pub interp: Interp,
    /// Subtract the ZODI model (`IMAGE - ZODI`) while resampling.
    /// SPHEREx `IMAGE` is not zodi-subtracted; the default `true`
    /// matches difference/stack modes, raw display passes `false`.
    /// A NaN ZODI sample leaves the flux untouched.
    pub subtract_zodi: bool,
}

impl Default for ReprojectOptions {
    fn default() -> Self {
        Self {
            interp: Interp::Bilinear,
            subtract_zodi: true,
        }
    }
}

/// Resample `frame` onto `grid`.
///
/// The result inherits `detector`, epoch (`MJD-AVG`, else `MJD-OBS`) and
/// a representative wavelength: the WCS-WAVE value at the input pixel
/// mapping back to the output grid's centre.
pub fn reproject(
    frame: &Frame,
    grid: &OutputGrid,
    opts: &ReprojectOptions,
) -> Result<ScienceImage, ScienceError> {
    let planes = Planes::of(frame);
    let proj = OutProj::new(&grid.wcs);
    let lin = InLin::new(&frame.wcs);
    let margin = sip_margin(frame, grid, &proj, &lin);

    let render = |oy: usize| render_row(oy, &planes, grid, opts, &proj, &lin, margin);
    let rows: Vec<Row> = if grid.nx * grid.ny >= PAR_MIN_PIXELS {
        (0..grid.ny).into_par_iter().map(render).collect()
    } else {
        (0..grid.ny).map(render).collect()
    };

    let (ny, nx) = (grid.ny, grid.nx);
    let npix = ny * nx;
    let (mut image, mut variance, mut flags, mut n_contrib) = (
        Vec::with_capacity(npix),
        Vec::with_capacity(npix),
        Vec::with_capacity(npix),
        Vec::with_capacity(npix),
    );
    for r in &rows {
        image.extend_from_slice(&r.image);
        variance.extend_from_slice(&r.variance);
        flags.extend_from_slice(&r.flags);
        n_contrib.extend_from_slice(&r.n_contrib);
    }
    let shape_err = || ScienceError::InvalidGrid("output raster assembly");
    let mut out = ScienceImage {
        grid: *grid,
        image: Array2::from_shape_vec((ny, nx), image).map_err(|_| shape_err())?,
        variance: Array2::from_shape_vec((ny, nx), variance).map_err(|_| shape_err())?,
        flags: Array2::from_shape_vec((ny, nx), flags).map_err(|_| shape_err())?,
        n_contrib: Array2::from_shape_vec((ny, nx), n_contrib).map_err(|_| shape_err())?,
        detector: frame.header.detector,
        wavelength_um: f64::NAN,
        mjd: frame.header.mjd_avg.or(frame.header.mjd_obs),
        zodi_subtracted: opts.subtract_zodi,
    };

    let (cra, cdec) = proj.radec(grid.nx as f64 / 2.0 + 0.5, grid.ny as f64 / 2.0 + 0.5);
    out.wavelength_um = frame
        .wcs
        .radec_to_pixel(cra, cdec)
        .map(|(sx, sy)| frame.wavelength.wavelength_at(sx, sy))
        .unwrap_or(f64::NAN);
    Ok(out)
}

/// Output pixels of one row.
struct Row {
    image: Vec<f32>,
    variance: Vec<f32>,
    flags: Vec<i32>,
    n_contrib: Vec<u16>,
}

/// Render output row `oy` (0-based): identical per-pixel math to the
/// original serial implementation.
fn render_row(
    oy: usize,
    planes: &Planes<'_>,
    grid: &OutputGrid,
    opts: &ReprojectOptions,
    proj: &OutProj,
    lin: &InLin,
    margin: f64,
) -> Row {
    let (nx_in, ny_in) = (planes.nx, planes.ny);
    let mut row = Row {
        image: Vec::with_capacity(grid.nx),
        variance: Vec::with_capacity(grid.nx),
        flags: Vec::with_capacity(grid.nx),
        n_contrib: Vec::with_capacity(grid.nx),
    };
    let y = oy as f64 + 1.0;
    for ox in 0..grid.nx {
        let (ra, dec) = proj.radec(ox as f64 + 1.0, y);
        let in_reach = match lin.estimate(ra, dec) {
            // The estimate can only differ from the exact pixel by the
            // SIP displacement (< `margin`), so outside-by-margin means
            // provably outside the footprint.
            Some((ex, ey)) => {
                ex >= 0.5 - margin
                    && ex <= nx_in as f64 + 0.5 + margin
                    && ey >= 0.5 - margin
                    && ey <= ny_in as f64 + 0.5 + margin
            }
            // Behind the tangent plane: `radec_to_pixel` errors too.
            None => false,
        };
        let mut val = f32::NAN;
        let (mut var, mut fl, mut n) = (f32::NAN, 0i32, 0u16);
        if in_reach {
            if let Ok((sx, sy)) = planes.wcs.radec_to_pixel(ra, dec) {
                if let Some((v, va, f)) = planes.sample(sx, sy, opts.interp) {
                    val = v;
                    var = va;
                    fl = f;
                    if opts.subtract_zodi {
                        let z = match opts.interp {
                            Interp::Nearest => planes.nearest_zodi(sx, sy),
                            Interp::Bilinear => planes.bilinear_zodi(sx, sy),
                        };
                        if z.is_finite() {
                            val -= z;
                        }
                    }
                    n = u16::from(val.is_finite());
                }
            }
        }
        row.image.push(val);
        row.variance.push(var);
        row.flags.push(fl);
        row.n_contrib.push(n);
    }
    row
}

/// Largest plausible SIP displacement in input pixels, measured per
/// frame by evaluating the exact inverse at a coarse output lattice and
/// comparing with the SIP-free linear estimate. SIP polynomials vary
/// smoothly across the small footprint, so three times the observed
/// maximum plus one pixel is a safe rejection bound; a frame without
/// SIP measures zero and the margin degenerates to the safety term.
fn sip_margin(frame: &Frame, grid: &OutputGrid, proj: &OutProj, lin: &InLin) -> f64 {
    let mut m = 0.0f64;
    'lattice: for iy in 0..MARGIN_LATTICE {
        for ix in 0..MARGIN_LATTICE {
            let x = 1.0 + ix as f64 * (grid.nx - 1).max(1) as f64 / (MARGIN_LATTICE - 1) as f64;
            let y = 1.0 + iy as f64 * (grid.ny - 1).max(1) as f64 / (MARGIN_LATTICE - 1) as f64;
            let (ra, dec) = proj.radec(x, y);
            let Some(est) = lin.estimate(ra, dec) else {
                continue;
            };
            let Ok(exact) = frame.wcs.radec_to_pixel(ra, dec) else {
                continue;
            };
            m = m.max((exact.0 - est.0).abs()).max((exact.1 - est.1).abs());
            // A distortion this large is beyond anything the margin can
            // safely bound; keep the whole call conservative instead.
            if m > 64.0 {
                break 'lattice;
            }
        }
    }
    3.0 * m + 1.0
}

/// Random access to a 2-D f32 plane: a flat slice when the layout is
/// standard (always true for parsed frames), else the `Array2` view.
#[derive(Clone, Copy)]
enum F32Plane<'a> {
    /// Contiguous row-major storage plus the row stride.
    Flat(&'a [f32], usize),
    /// Generic indexing fallback for non-standard layouts.
    D2(&'a Array2<f32>),
}

impl<'a> F32Plane<'a> {
    #[inline]
    fn at(&self, iy: usize, ix: usize) -> f32 {
        match self {
            Self::Flat(s, stride) => s[iy * stride + ix],
            Self::D2(a) => a[[iy, ix]],
        }
    }
}

/// Same for the i32 FLAGS plane.
#[derive(Clone, Copy)]
enum I32Plane<'a> {
    /// Contiguous row-major storage plus the row stride.
    Flat(&'a [i32], usize),
    /// Generic indexing fallback for non-standard layouts.
    D2(&'a Array2<i32>),
}

impl<'a> I32Plane<'a> {
    #[inline]
    fn at(&self, iy: usize, ix: usize) -> i32 {
        match self {
            Self::Flat(s, stride) => s[iy * stride + ix],
            Self::D2(a) => a[[iy, ix]],
        }
    }
}

/// The input frame's sampling planes plus its WCS.
struct Planes<'a> {
    wcs: &'a SkyWcs,
    image: F32Plane<'a>,
    variance: F32Plane<'a>,
    zodi: F32Plane<'a>,
    flags: I32Plane<'a>,
    nx: usize,
    ny: usize,
}

fn f32_plane<'a>(a: &'a Array2<f32>) -> F32Plane<'a> {
    match a.as_slice() {
        Some(s) => F32Plane::Flat(s, a.ncols()),
        None => F32Plane::D2(a),
    }
}

fn i32_plane<'a>(a: &'a Array2<i32>) -> I32Plane<'a> {
    match a.as_slice() {
        Some(s) => I32Plane::Flat(s, a.ncols()),
        None => I32Plane::D2(a),
    }
}

impl<'a> Planes<'a> {
    fn of(frame: &'a Frame) -> Self {
        let (ny, nx) = frame.image.dim();
        Self {
            wcs: &frame.wcs,
            image: f32_plane(&frame.image),
            variance: f32_plane(&frame.variance),
            zodi: f32_plane(&frame.zodi),
            flags: i32_plane(&frame.flags),
            nx,
            ny,
        }
    }

    /// Sample the frame at source coords `(sx, sy)` -> `(flux, variance,
    /// flags)`; `None` when outside the footprint.
    #[inline]
    fn sample(&self, sx: f64, sy: f64, interp: Interp) -> Option<(f32, f32, i32)> {
        let (nx, ny) = (self.nx, self.ny);
        if !in_footprint(sx, sy, nx, ny) {
            return None;
        }
        let flags = self.flags.at(nearest_idx(sy, ny), nearest_idx(sx, nx));
        let (val, var) = match interp {
            Interp::Nearest => (
                self.image.at(nearest_idx(sy, ny), nearest_idx(sx, nx)),
                self.variance.at(nearest_idx(sy, ny), nearest_idx(sx, nx)),
            ),
            Interp::Bilinear => (
                bilinear_f32(self.image, sx, sy, nx, ny),
                bilinear_var(self.variance, sx, sy, nx, ny),
            ),
        };
        Some((val, var, flags))
    }

    #[inline]
    fn nearest_zodi(&self, sx: f64, sy: f64) -> f32 {
        self.zodi
            .at(nearest_idx(sy, self.ny), nearest_idx(sx, self.nx))
    }

    #[inline]
    fn bilinear_zodi(&self, sx: f64, sy: f64) -> f32 {
        bilinear_f32(self.zodi, sx, sy, self.nx, self.ny)
    }
}

/// Precomputed reference-point terms of the output grid's linear TAN
/// projection. [`Self::radec`] evaluates the same expression as
/// [`LinearTanWcs::pixel_to_radec`], hoisting the `crval` trig that the
/// original recomputed per pixel; results are bitwise identical.
struct OutProj {
    crpix: [f64; 2],
    cd: [[f64; 2]; 2],
    n0: [f64; 3],
    e_ra: [f64; 3],
    e_dec: [f64; 3],
}

impl OutProj {
    fn new(w: &LinearTanWcs) -> Self {
        let (ra0, dec0) = (w.crval[0] * DEG, w.crval[1] * DEG);
        Self {
            crpix: w.crpix,
            cd: w.cd,
            n0: [dec0.cos() * ra0.cos(), dec0.cos() * ra0.sin(), dec0.sin()],
            e_ra: [-ra0.sin(), ra0.cos(), 0.0],
            e_dec: [-dec0.sin() * ra0.cos(), -dec0.sin() * ra0.sin(), dec0.cos()],
        }
    }

    #[inline]
    fn radec(&self, x: f64, y: f64) -> (f64, f64) {
        let u = x - self.crpix[0];
        let v = y - self.crpix[1];
        let xi = (self.cd[0][0] * u + self.cd[0][1] * v) * DEG;
        let eta = (self.cd[1][0] * u + self.cd[1][1] * v) * DEG;
        let d = [
            self.n0[0] + xi * self.e_ra[0] + eta * self.e_dec[0],
            self.n0[1] + xi * self.e_ra[1] + eta * self.e_dec[1],
            self.n0[2] + xi * self.e_ra[2] + eta * self.e_dec[2],
        ];
        let ra = d[1].atan2(d[0]).to_degrees().rem_euclid(360.0);
        let dec = (d[2] / norm(d)).asin().to_degrees();
        (ra, dec)
    }
}

/// Precomputed SIP-free part of the input frame's inverse WCS: the
/// direction cosines plus the inverse CD of
/// `SkyWcs::radec_to_pixel`'s preamble. [`Self::estimate`] returns the
/// exact pre-distortion pixel coordinate (`up`, `vp` in that code) — the
/// exact answer whenever the frame carries no SIP terms.
struct InLin {
    crpix: [f64; 2],
    cd: [[f64; 2]; 2],
    n0: [f64; 3],
    e_ra: [f64; 3],
    e_dec: [f64; 3],
}

impl InLin {
    fn new(w: &SkyWcs) -> Self {
        let (ra0, dec0) = (w.crval[0] * DEG, w.crval[1] * DEG);
        Self {
            crpix: w.crpix,
            cd: w.cd,
            n0: [dec0.cos() * ra0.cos(), dec0.cos() * ra0.sin(), dec0.sin()],
            e_ra: [-ra0.sin(), ra0.cos(), 0.0],
            e_dec: [-dec0.sin() * ra0.cos(), -dec0.sin() * ra0.sin(), dec0.cos()],
        }
    }

    /// SIP-free pixel estimate, `None` exactly when the point is behind
    /// the tangent plane (the same condition `radec_to_pixel` errors
    /// on).
    #[inline]
    fn estimate(&self, ra: f64, dec: f64) -> Option<(f64, f64)> {
        let (ra_r, dec_r) = (ra * DEG, dec * DEG);
        let d = [
            dec_r.cos() * ra_r.cos(),
            dec_r.cos() * ra_r.sin(),
            dec_r.sin(),
        ];
        let denom = dot(d, self.n0);
        if denom <= 0.0 {
            return None;
        }
        let xi = (dot(d, self.e_ra) / denom).to_degrees();
        let eta = (dot(d, self.e_dec) / denom).to_degrees();
        let det = self.cd[0][0] * self.cd[1][1] - self.cd[0][1] * self.cd[1][0];
        let up = (self.cd[1][1] * xi - self.cd[0][1] * eta) / det;
        let vp = (-self.cd[1][0] * xi + self.cd[0][0] * eta) / det;
        Some((up + self.crpix[0], vp + self.crpix[1]))
    }
}

/// Footprint test: sample positions with centre coverage of the array.
/// `(sx, sy)` are 1-based source pixel coordinates.
fn in_footprint(sx: f64, sy: f64, nx: usize, ny: usize) -> bool {
    sx.is_finite()
        && sy.is_finite()
        && sx >= 0.5
        && sx <= nx as f64 + 0.5
        && sy >= 0.5
        && sy <= ny as f64 + 0.5
}

/// 0-based index of the nearest pixel centre to 1-based coord `s`.
fn nearest_idx(s: f64, n: usize) -> usize {
    (s.round() as i64).clamp(1, n as i64) as usize - 1
}

/// Bilinear sample of an f32 plane: coefficients renormalized over the
/// finite neighbours; NaN when none are finite. Same evaluation order
/// as the original `bilinear_cells` version.
fn bilinear_f32(a: F32Plane<'_>, sx: f64, sy: f64, nx: usize, ny: usize) -> f32 {
    let jx = sx.floor() as i64;
    let jy = sy.floor() as i64;
    let fx = sx - jx as f64;
    let fy = sy - jy as f64;
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for (iy, wy) in [(jy, 1.0 - fy), (jy + 1, fy)] {
        for (ix, wx) in [(jx, 1.0 - fx), (jx + 1, fx)] {
            if (1..=nx as i64).contains(&ix) && (1..=ny as i64).contains(&iy) {
                let v = a.at((iy - 1) as usize, (ix - 1) as usize);
                if v.is_finite() {
                    num += wx * wy * f64::from(v);
                    den += wx * wy;
                }
            }
        }
    }
    if den > 0.0 {
        (num / den) as f32
    } else {
        f32::NAN
    }
}

/// Bilinear variance propagation `sum(c_i^2 v_i)` over neighbours with
/// finite, positive variance (coefficients renormalized on that subset).
fn bilinear_var(a: F32Plane<'_>, sx: f64, sy: f64, nx: usize, ny: usize) -> f32 {
    let jx = sx.floor() as i64;
    let jy = sy.floor() as i64;
    let fx = sx - jx as f64;
    let fy = sy - jy as f64;
    let (mut acc, mut den) = (0.0f64, 0.0f64);
    for (iy, wy) in [(jy, 1.0 - fy), (jy + 1, fy)] {
        for (ix, wx) in [(jx, 1.0 - fx), (jx + 1, fx)] {
            if (1..=nx as i64).contains(&ix) && (1..=ny as i64).contains(&iy) {
                let c = wx * wy;
                let v = a.at((iy - 1) as usize, (ix - 1) as usize);
                if v.is_finite() && v > 0.0 {
                    acc += c * c * f64::from(v);
                    den += c;
                }
            }
        }
    }
    if den > 0.0 {
        (acc / (den * den)) as f32
    } else {
        f32::NAN
    }
}

fn norm(d: [f64; 3]) -> f64 {
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
