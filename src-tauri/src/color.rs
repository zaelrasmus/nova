//! Perceptual color extraction and matching.
//!
//! Palettes are stored in CIELAB, not sRGB, because Euclidean distance in LAB
//! approximates *perceived* difference. In RGB it doesn't: two greens can be
//! numerically further apart than a green and a grey, which makes naive RGB
//! color search feel broken.
//!
//! A palette, not a single dominant color. A sunset that is 60% blue sky and 30%
//! orange has exactly one dominant color — blue — and a single-color model can
//! never find it by searching orange, even though a third of the image is.

use image::DynamicImage;
use std::collections::HashMap;

/// Colors kept per asset. Deliberately generous: neutrals count as real colors
/// (a product shot on white legitimately has white on top), so the subject's
/// color needs room to survive underneath the background's.
const PALETTE_SIZE: usize = 8;

/// Entries covering less than this share of the image are dropped as noise.
const MIN_COVERAGE: f32 = 0.03;

/// Bins closer than this (ΔE) merge, so a palette doesn't spend five of its eight
/// slots on five shades of the same sky and lose the subject entirely.
const MERGE_DELTA_E: f32 = 12.0;

/// Quantization steps. Coarse on purpose — the goal is grouping, and the merge
/// pass repairs whatever lands either side of a bin edge.
const L_STEP: f32 = 10.0;
const AB_STEP: f32 = 16.0;

/// Pixels below this alpha don't contribute (transparent PNG margins, stickers).
const MIN_ALPHA: u8 = 16;

/// Longest edge used for sampling. ~9k pixels is ample for a palette and keeps
/// extraction in the low single-digit milliseconds — the reason this can ride
/// along with thumbnail generation for free.
const SAMPLE_EDGE: u32 = 96;

/// Chroma at which the lightness weight bottoms out (see `lightness_weight`).
const CHROMA_FULL: f32 = 60.0;
/// Lightness weight for a fully saturated target.
const MIN_L_WEIGHT: f32 = 0.4;

/// Quantization bin key: a coarse (L, a, b) cell.
type BinKey = (i32, i32, i32);
/// Running LAB sum plus pixel count for one bin.
type BinAccum = (f32, f32, f32, u32);

#[derive(Clone, Copy, Debug)]
pub struct Lab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

/// One palette entry: a color plus the share of the image it covers (0.0–1.0).
#[derive(Clone, Copy, Debug)]
pub struct PaletteEntry {
    pub lab: Lab,
    pub ratio: f32,
}

/// D65 white point, the reference both conversions normalize against.
const XN: f32 = 0.950_47;
const YN: f32 = 1.0;
const ZN: f32 = 1.088_83;

/// Break point of the CIE curve's linear segment, which keeps it finite at zero.
const DELTA: f32 = 6.0 / 29.0;

/// sRGB (0–255) to CIELAB, D65 white point.
pub fn srgb_to_lab(r: u8, g: u8, b: u8) -> Lab {
    // 1. Undo the sRGB transfer function to get linear light.
    fn linearize(c: u8) -> f32 {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    let (r, g, b) = (linearize(r), linearize(g), linearize(b));

    // 2. Linear RGB -> XYZ (sRGB primaries, D65).
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b;
    let z = 0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b;

    // 3. XYZ -> Lab, normalized against the D65 white point.
    fn f(t: f32) -> f32 {
        if t > DELTA * DELTA * DELTA {
            t.cbrt()
        } else {
            t / (3.0 * DELTA * DELTA) + 4.0 / 29.0
        }
    }
    let (fx, fy, fz) = (f(x / XN), f(y / YN), f(z / ZN));

    Lab {
        l: 116.0 * fy - 16.0,
        a: 500.0 * (fx - fy),
        b: 200.0 * (fy - fz),
    }
}

/// CIELAB back to sRGB (0–255), for showing a stored palette entry on screen.
///
/// Lab describes more colors than sRGB can display, and averaging pixels inside a
/// quantization bin can land just outside the gamut, so clamping is normal here
/// rather than a sign of bad input — the nearest displayable color is exactly
/// what a swatch should show.
pub fn lab_to_srgb(lab: Lab) -> (u8, u8, u8) {
    // 1. Lab -> XYZ (the inverse of `f` above).
    fn f_inv(t: f32) -> f32 {
        if t > DELTA {
            t * t * t
        } else {
            3.0 * DELTA * DELTA * (t - 4.0 / 29.0)
        }
    }
    let fy = (lab.l + 16.0) / 116.0;
    let fx = fy + lab.a / 500.0;
    let fz = fy - lab.b / 200.0;
    let (x, y, z) = (XN * f_inv(fx), YN * f_inv(fy), ZN * f_inv(fz));

    // 2. XYZ -> linear RGB (inverse of the sRGB/D65 matrix used above).
    let r = 3.240_97 * x - 1.537_383 * y - 0.498_611 * z;
    let g = -0.969_244 * x + 1.875_968 * y + 0.041_555 * z;
    let b = 0.055_63 * x - 0.203_977 * y + 1.056_972 * z;

    // 3. Re-apply the sRGB transfer function.
    fn encode(c: f32) -> u8 {
        let c = c.clamp(0.0, 1.0);
        let s = if c <= 0.003_130_8 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0).round() as u8
    }
    (encode(r), encode(g), encode(b))
}

/// Weighted squared distance between two LAB colors. Squared so callers can
/// compare against a squared tolerance and skip the square root entirely — which
/// also keeps the SQL form to plain multiply-and-add.
pub fn dist_sq(x: Lab, y: Lab, l_weight: f32) -> f32 {
    let dl = x.l - y.l;
    let da = x.a - y.a;
    let db = x.b - y.b;
    l_weight * dl * dl + da * da + db * db
}

/// How much the lightness axis should count when matching against `target`.
///
/// Plain Euclidean LAB distance treats "dark red" as far from "red", so a search
/// for red misses most actual reds. Downweighting L fixes that — but a NEUTRAL
/// target is *nothing but* lightness, so the same downweighting would make a
/// mid-grey search also return black and white.
///
/// So the weight follows the target's own chroma: full weight for neutrals,
/// reduced for saturated colors. Computed here and bound as a query parameter,
/// so the SQL stays arithmetic.
pub fn lightness_weight(target: Lab) -> f32 {
    let chroma = (target.a * target.a + target.b * target.b).sqrt();
    let t = (chroma / CHROMA_FULL).clamp(0.0, 1.0);
    1.0 - (1.0 - MIN_L_WEIGHT) * t
}

/// Extract the dominant colors of `img` with their coverage ratios.
///
/// Quantize-and-histogram rather than k-means: at this sample size the grouping
/// is comparable and it costs one pass instead of several iterations.
pub fn extract_palette(img: &DynamicImage) -> Vec<PaletteEntry> {
    let small = img.thumbnail(SAMPLE_EDGE, SAMPLE_EDGE).to_rgba8();

    // Accumulate a running LAB sum per bin, so each bin's color ends up the true
    // mean of its pixels rather than the bin's midpoint.
    let mut bins: HashMap<BinKey, BinAccum> = HashMap::new();
    let mut counted: u32 = 0;

    for px in small.pixels() {
        if px[3] < MIN_ALPHA {
            continue;
        }
        let lab = srgb_to_lab(px[0], px[1], px[2]);
        let key = (
            (lab.l / L_STEP).round() as i32,
            (lab.a / AB_STEP).round() as i32,
            (lab.b / AB_STEP).round() as i32,
        );
        let slot = bins.entry(key).or_insert((0.0, 0.0, 0.0, 0));
        slot.0 += lab.l;
        slot.1 += lab.a;
        slot.2 += lab.b;
        slot.3 += 1;
        counted += 1;
    }

    if counted == 0 {
        return Vec::new(); // fully transparent
    }

    let mut entries: Vec<(Lab, u32)> = bins
        .into_values()
        .map(|(sl, sa, sb, n)| {
            let n_f = n as f32;
            (
                Lab {
                    l: sl / n_f,
                    a: sa / n_f,
                    b: sb / n_f,
                },
                n,
            )
        })
        .collect();

    // Biggest first, so merging always folds a smaller bin into a larger one.
    entries.sort_unstable_by(|x, y| y.1.cmp(&x.1));

    let merge_sq = MERGE_DELTA_E * MERGE_DELTA_E;
    let mut merged: Vec<(Lab, u32)> = Vec::new();
    for (lab, n) in entries {
        // Unweighted (l_weight = 1.0): merging is about the colors being the same,
        // not about how a particular search should treat them.
        match merged
            .iter_mut()
            .find(|(m, _)| dist_sq(*m, lab, 1.0) <= merge_sq)
        {
            Some(slot) => {
                let total = (slot.1 + n) as f32;
                let (w_old, w_new) = (slot.1 as f32, n as f32);
                slot.0 = Lab {
                    l: (slot.0.l * w_old + lab.l * w_new) / total,
                    a: (slot.0.a * w_old + lab.a * w_new) / total,
                    b: (slot.0.b * w_old + lab.b * w_new) / total,
                };
                slot.1 += n;
            }
            None => merged.push((lab, n)),
        }
    }

    // Merging shifts counts, so re-sort before taking the top entries.
    merged.sort_unstable_by(|x, y| y.1.cmp(&x.1));
    merged
        .into_iter()
        .map(|(lab, n)| PaletteEntry {
            lab,
            ratio: n as f32 / counted as f32,
        })
        .filter(|e| e.ratio >= MIN_COVERAGE)
        .take(PALETTE_SIZE)
        .collect()
}
