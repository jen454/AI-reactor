//! Menu bar artwork.
//!
//! # A placeholder for the icon pack system
//!
//! The spec makes icon packs the app's most important design decision: packs
//! live as data under `icons/`, the app scans them, and adding one must need
//! no code change. That system is milestone 6.
//!
//! Until then the menu bar still needs something to draw, so this file
//! rasterises the default pack's shape directly. **It is not the pack system
//! and must not grow into one.** When milestone 6 lands, this becomes one
//! `manifest.json` and its own assets.
//!
//! # From quantised levels to a live number (2026-09-17)
//!
//! This pack used to show six discrete steps — L0..L5 — because a continuous
//! value was assumed unreadable at 16–22pt: "the difference between 71% and
//! 68% cannot be drawn at that size." That assumption held for a *shape*
//! (panel count, needle angle). It does not hold once the number is also
//! shown as real text (the status item's own title, in `menubar/driver.rs`),
//! so there is no longer a reason to throw the precision away.
//!
//! # A ring plus a logo, back to a template image (2026-09-17)
//!
//! Two earlier passes this same day gave each provider's icon a fixed colour
//! — first a per-provider brand colour, then a shared macOS-style green ring
//! — so two subscriptions could be told apart. Asked for a third time: drop
//! the fixed colour entirely and go back to a real macOS template image
//! (alpha only, tinted white or black by the system to match the bar), with
//! the provider now told apart by a logo mark drawn in the centre of the
//! ring instead of by hue. That means "how much is left" goes back to being
//! carried by *alpha* (a faint track, a fully opaque fill) rather than by two
//! different colours, and a small hand-built SVG-path rasteriser draws the
//! two marks — see [`Mark`] and `parse_svg_path` below. Nothing here reads a
//! `.svg` file at runtime; the two path strings are copied in as constants
//! from the same assets the popover uses, so there is exactly one on-disk
//! source of truth for each mark's shape even though it is rasterised twice,
//! once by WebKit and once by this file.

/// Shown at 24pt tall (see `platform::set_tray_image_size`); we draw at 2x
/// for Retina. Grown from 22pt when asked for a bigger icon — the gauge
/// already filled nearly all of its 22pt canvas, so the canvas itself had to
/// grow. If a bar is shorter than this, macOS scales the image down to fit
/// rather than clipping it. Square, so the arc is a true circle.
pub const HEIGHT: u32 = 48;
pub const WIDTH: u32 = 48;

/// Shapes are drawn at 4x and box-filtered down — the cheapest antialiasing
/// without a graphics dependency, and it runs only when the reading changes.
const SS: usize = 4;

/// Template artwork carries no colour of its own; macOS supplies it.
const INK: [u8; 3] = [0, 0, 0];

/// Which provider's logo sits in the centre of the ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Claude,
    Codex,
}

/// A coverage buffer at supersampled resolution.
///
/// Despite the name this doubles as an *alpha* buffer, not just a binary
/// mask: a value below 1.0 can mean "this subsample sits on an antialiased
/// edge" or "this shape was deliberately drawn at partial opacity" (the ring's
/// unfilled track). `resolve` treats both the same way, which is correct —
/// both are just "how opaque should this pixel end up."
struct Canvas {
    w: usize,
    h: usize,
    cov: Vec<f32>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w: w * SS,
            h: h * SS,
            cov: vec![0.0; w * SS * h * SS],
        }
    }

    /// A filled ring segment: the region between two radii and two angles,
    /// painted at `alpha` (1.0 for a fully lit fill, less for a faint track).
    /// Angles in degrees, clockwise from east in screen space (y down).
    ///
    /// The panels this used to draw were strokes — dots stamped along an arc
    /// path (`radius`, constant, plus a `width`), so each panel's short edges
    /// were round caps rather than a flat cut. That version had a real bug:
    /// the stamped dot's radius was passed `width` directly instead of
    /// `width / 2`, silently doubling every stroke. It went unnoticed while
    /// every arc drawn that way was a full circle (a track, or one continuous
    /// fill) — "twice as thick as intended" just reads as "a bit bolder" —
    /// and stopped being invisible the moment two arcs had to leave a gap
    /// between them.
    ///
    /// This fills a true trapezoid instead — flat radial edges, sharp corners
    /// where they meet the inner and outer arcs — which sidesteps that whole
    /// class of bug (there is no cap to mis-size).
    fn annular_wedge(&mut self, cx: f32, cy: f32, r_inner: f32, r_outer: f32, from_deg: f32, to_deg: f32, alpha: f32) {
        let lo_x = ((cx - r_outer) * SS as f32).floor().max(0.0) as usize;
        let lo_y = ((cy - r_outer) * SS as f32).floor().max(0.0) as usize;
        let hi_x = (((cx + r_outer) * SS as f32).ceil().min(self.w as f32)) as usize;
        let hi_y = (((cy + r_outer) * SS as f32).ceil().min(self.h as f32)) as usize;

        for py in lo_y..hi_y {
            for px in lo_x..hi_x {
                let sx = (px as f32 + 0.5) / SS as f32;
                let sy = (py as f32 + 0.5) / SS as f32;
                let (dx, dy) = (sx - cx, sy - cy);
                let r = (dx * dx + dy * dy).sqrt();
                if r < r_inner || r > r_outer {
                    continue;
                }
                let mut angle = dy.atan2(dx).to_degrees();
                if angle < 0.0 {
                    angle += 360.0;
                }
                // `from_deg`/`to_deg` can run past 360 (this file builds them
                // from `RING_START`, which is already 270), while `angle` is
                // normalised to 0..360 — so also try the pixel's angle one lap
                // further round before ruling it out.
                if (from_deg..=to_deg).contains(&angle)
                    || (from_deg..=to_deg).contains(&(angle + 360.0))
                {
                    self.cov[py * self.w + px] = alpha;
                }
            }
        }
    }

    /// A filled circle at `alpha` — the rounded end caps on the gauge arc.
    fn disc(&mut self, cx: f32, cy: f32, r: f32, alpha: f32) {
        let lo_x = ((cx - r) * SS as f32).floor().max(0.0) as usize;
        let lo_y = ((cy - r) * SS as f32).floor().max(0.0) as usize;
        let hi_x = (((cx + r) * SS as f32).ceil().min(self.w as f32)) as usize;
        let hi_y = (((cy + r) * SS as f32).ceil().min(self.h as f32)) as usize;
        for py in lo_y..hi_y {
            for px in lo_x..hi_x {
                let dx = (px as f32 + 0.5) / SS as f32 - cx;
                let dy = (py as f32 + 0.5) / SS as f32 - cy;
                if dx * dx + dy * dy <= r * r {
                    self.cov[py * self.w + px] = alpha;
                }
            }
        }
    }

    /// Fill the region enclosed by `subpaths` (even-odd rule) at `alpha`.
    /// Used only for the logo marks — see `parse_svg_path`.
    fn fill_path(&mut self, subpaths: &[Vec<(f32, f32)>], alpha: f32) {
        let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for poly in subpaths {
            for &(x, y) in poly {
                lo_x = lo_x.min(x);
                lo_y = lo_y.min(y);
                hi_x = hi_x.max(x);
                hi_y = hi_y.max(y);
            }
        }
        if lo_x > hi_x {
            return;
        }
        let lo_x_px = (lo_x * SS as f32).floor().max(0.0) as usize;
        let lo_y_px = (lo_y * SS as f32).floor().max(0.0) as usize;
        let hi_x_px = ((hi_x * SS as f32).ceil().min(self.w as f32)) as usize;
        let hi_y_px = ((hi_y * SS as f32).ceil().min(self.h as f32)) as usize;

        for py in lo_y_px..hi_y_px {
            for px in lo_x_px..hi_x_px {
                let sx = (px as f32 + 0.5) / SS as f32;
                let sy = (py as f32 + 0.5) / SS as f32;
                if point_in_polygons(subpaths, sx, sy) {
                    self.cov[py * self.w + px] = alpha;
                }
            }
        }
    }

    fn resolve(&self, w: usize, h: usize) -> Vec<f32> {
        let mut out = vec![0.0; w * h];
        let per_pixel = (SS * SS) as f32;
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0.0;
                for sy in 0..SS {
                    for sx in 0..SS {
                        sum += self.cov[(y * SS + sy) * self.w + (x * SS + sx)];
                    }
                }
                out[y * w + x] = sum / per_pixel;
            }
        }
        out
    }
}

/// A minimal parser for the one shape of path data both logo marks actually
/// use: absolute `M`/`L`/`H`/`V`/`C`/`Z` commands, numbers separated by
/// whitespace. This is not a general SVG path parser — no relative commands,
/// no shorthand curve commands, no arcs — and must not grow into one; if a
/// future mark needs more of the grammar, reach for a real SVG crate instead
/// of extending this by hand.
fn parse_svg_path(d: &str) -> Vec<Vec<(f32, f32)>> {
    enum Token {
        Cmd(char),
        Num(f32),
    }

    let mut tokens = Vec::new();
    let mut num = String::new();
    for c in d.chars() {
        if c.is_ascii_alphabetic() {
            if let Ok(v) = num.parse() {
                tokens.push(Token::Num(v));
            }
            num.clear();
            tokens.push(Token::Cmd(c));
        } else if c == '-' && !num.is_empty() {
            if let Ok(v) = num.parse() {
                tokens.push(Token::Num(v));
            }
            num.clear();
            num.push(c);
        } else if c.is_ascii_digit() || c == '.' || c == '-' {
            num.push(c);
        } else if let Ok(v) = num.parse() {
            tokens.push(Token::Num(v));
            num.clear();
        }
    }
    if let Ok(v) = num.parse() {
        tokens.push(Token::Num(v));
    }

    let nums = |tokens: &[Token], i: &mut usize, n: usize| -> Vec<f32> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            if let Some(Token::Num(v)) = tokens.get(*i) {
                out.push(*v);
                *i += 1;
            }
        }
        out
    };

    let mut subpaths = Vec::new();
    let mut current: Vec<(f32, f32)> = Vec::new();
    let mut pos = (0.0f32, 0.0f32);
    let mut start = pos;
    let mut i = 0;
    while i < tokens.len() {
        let cmd = match tokens[i] {
            Token::Cmd(c) => c,
            Token::Num(_) => {
                i += 1;
                continue;
            }
        };
        i += 1;
        match cmd {
            'M' => {
                let v = nums(&tokens, &mut i, 2);
                if !current.is_empty() {
                    subpaths.push(std::mem::take(&mut current));
                }
                pos = (v[0], v[1]);
                start = pos;
                current.push(pos);
            }
            'L' => {
                let v = nums(&tokens, &mut i, 2);
                pos = (v[0], v[1]);
                current.push(pos);
            }
            'H' => {
                let v = nums(&tokens, &mut i, 1);
                pos = (v[0], pos.1);
                current.push(pos);
            }
            'V' => {
                let v = nums(&tokens, &mut i, 1);
                pos = (pos.0, v[0]);
                current.push(pos);
            }
            'C' => {
                let v = nums(&tokens, &mut i, 6);
                flatten_cubic(pos, (v[0], v[1]), (v[2], v[3]), (v[4], v[5]), &mut current);
                pos = (v[4], v[5]);
            }
            'Z' => {
                pos = start;
            }
            _ => {}
        }
    }
    if !current.is_empty() {
        subpaths.push(current);
    }
    subpaths
}

/// Subdivide a cubic Bézier into line segments and append them.
fn flatten_cubic(p0: (f32, f32), c1: (f32, f32), c2: (f32, f32), p1: (f32, f32), out: &mut Vec<(f32, f32)>) {
    const STEPS: usize = 12;
    for s in 1..=STEPS {
        let t = s as f32 / STEPS as f32;
        let mt = 1.0 - t;
        let x = mt * mt * mt * p0.0 + 3.0 * mt * mt * t * c1.0 + 3.0 * mt * t * t * c2.0 + t * t * t * p1.0;
        let y = mt * mt * mt * p0.1 + 3.0 * mt * mt * t * c1.1 + 3.0 * mt * t * t * c2.1 + t * t * t * p1.1;
        out.push((x, y));
    }
}

/// Even-odd point-in-polygon test across every subpath together, so a mark
/// built from overlapping loops (Codex's) gets its negative space right.
fn point_in_polygons(subpaths: &[Vec<(f32, f32)>], x: f32, y: f32) -> bool {
    let mut crossings = 0;
    for poly in subpaths {
        let n = poly.len();
        for i in 0..n {
            let (x0, y0) = poly[i];
            let (x1, y1) = poly[(i + 1) % n];
            if (y0 > y) != (y1 > y) {
                let x_at = x0 + (y - y0) / (y1 - y0) * (x1 - x0);
                if x < x_at {
                    crossings += 1;
                }
            }
        }
    }
    crossings % 2 == 1
}

/// Scale a freshly parsed path so its own bounding box (not its source
/// viewBox, which may have asymmetric padding) is centred at `center` and
/// its longer dimension equals `diameter`.
fn fit_path(subpaths: &mut [Vec<(f32, f32)>], center: (f32, f32), diameter: f32) {
    let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for poly in subpaths.iter() {
        for &(x, y) in poly {
            lo_x = lo_x.min(x);
            lo_y = lo_y.min(y);
            hi_x = hi_x.max(x);
            hi_y = hi_y.max(y);
        }
    }
    let scale = diameter / (hi_x - lo_x).max(hi_y - lo_y);
    let mid = ((lo_x + hi_x) / 2.0, (lo_y + hi_y) / 2.0);
    for poly in subpaths.iter_mut() {
        for p in poly.iter_mut() {
            p.0 = (p.0 - mid.0) * scale + center.0;
            p.1 = (p.1 - mid.1) * scale + center.1;
        }
    }
}

/// Claude's own asterisk mark, copied from `src/assets/claude.svg` (the same
/// file the popover renders). Single closed path, straight segments and a
/// handful of Béziers at the arm tips.
const CLAUDE_MARK_PATH: &str = "M52.4285 162.873L98.7844 136.879L99.5485 134.602L98.7844 133.334H96.4921L88.7237 132.862L62.2346 132.153L39.3113 131.207L17.0249 130.026L11.4214 128.844L6.2 121.873L6.7094 118.447L11.4214 115.257L18.171 115.847L33.0711 116.911L55.485 118.447L71.6586 119.392L95.728 121.873H99.5485L100.058 120.337L98.7844 119.392L97.7656 118.447L74.5877 102.732L49.4995 86.1905L36.3823 76.62L29.3779 71.7757L25.8121 67.2858L24.2839 57.3608L30.6515 50.2716L39.3113 50.8623L41.4763 51.4531L50.2636 58.1879L68.9842 72.7209L93.4357 90.6804L97.0015 93.6343L98.4374 92.6652L98.6571 91.9801L97.0015 89.2625L83.757 65.2772L69.621 40.8192L63.2534 30.6579L61.5978 24.632C60.9565 22.1032 60.579 20.0111 60.579 17.4246L67.8381 7.49965L71.9133 6.19995L81.7193 7.49965L85.7946 11.0443L91.9074 24.9865L101.714 46.8451L116.996 76.62L121.453 85.4816L123.873 93.6343L124.764 96.1155H126.292V94.6976L127.566 77.9197L129.858 57.3608L132.15 30.8942L132.915 23.4505L136.608 14.4708L143.994 9.62643L149.725 12.344L154.437 19.0788L153.8 23.4505L150.998 41.6463L145.522 70.1215L141.957 89.2625H143.994L146.414 86.7813L156.093 74.0206L172.266 53.698L179.398 45.6635L187.803 36.802L193.152 32.5484H203.34L210.726 43.6549L207.415 55.1159L196.972 68.3492L188.312 79.5739L175.896 96.2095L168.191 109.585L168.882 110.689L170.738 110.53L198.755 104.504L213.91 101.787L231.994 98.7149L240.144 102.496L241.036 106.395L237.852 114.311L218.495 119.037L195.826 123.645L162.07 131.592L161.696 131.893L162.137 132.547L177.36 133.925L183.855 134.279H199.774L229.447 136.524L237.215 141.605L241.8 147.867L241.036 152.711L229.065 158.737L213.019 154.956L175.45 145.977L162.587 142.787H160.805V143.85L171.502 154.366L191.242 172.089L215.82 195.011L217.094 200.682L213.91 205.172L210.599 204.699L188.949 188.394L180.544 181.069L161.696 165.118H160.422V166.772L164.752 173.152L187.803 207.771L188.949 218.405L187.294 221.832L181.308 223.959L174.813 222.777L161.187 203.754L147.305 182.486L136.098 163.345L134.745 164.2L128.075 235.42L125.019 239.082L117.887 241.8L111.902 237.31L108.718 229.984L111.902 215.452L115.722 196.547L118.779 181.541L121.58 162.873L123.291 156.636L123.14 156.219L121.773 156.449L107.699 175.752L86.304 204.699L69.3663 222.777L65.291 224.431L58.2867 220.768L58.9235 214.27L62.8713 208.48L86.304 178.705L100.44 160.155L109.551 149.507L109.462 147.967L108.959 147.924L46.6977 188.512L35.6182 189.93L30.7788 185.44L31.4156 178.115L33.7079 175.752L52.4285 162.873Z";

/// OpenAI's knot mark, copied from `src/assets/codex.svg` (the same file the
/// popover renders, minus the fill colour — this file is monochrome by
/// design; see the module docs).
const CODEX_MARK_PATH: &str = "M9.20509 8.76511V6.50545C9.20509 6.31513 9.27649 6.17234 9.44293 6.0773L13.9861 3.46088C14.6046 3.10413 15.342 2.93769 16.103 2.93769C18.9573 2.93769 20.7651 5.14983 20.7651 7.50454C20.7651 7.67098 20.7651 7.86129 20.7412 8.05161L16.0316 5.2924C15.7462 5.12596 15.4607 5.12596 15.1753 5.2924L9.20509 8.76511ZM19.8135 17.5659V12.1664C19.8135 11.8333 19.6708 11.5955 19.3854 11.429L13.4152 7.95633L15.3656 6.83833C15.5321 6.74328 15.6749 6.74328 15.8413 6.83833L20.3845 9.45474C21.6928 10.216 22.5728 11.8333 22.5728 13.4031C22.5728 15.2108 21.5025 16.8758 19.8135 17.5657V17.5659ZM7.80173 12.8088L5.8513 11.6671C5.68486 11.5721 5.61346 11.4293 5.61346 11.239V6.00613C5.61346 3.46111 7.56389 1.53433 10.2042 1.53433C11.2033 1.53433 12.1307 1.86743 12.9159 2.46202L8.2301 5.17371C7.94475 5.34015 7.80195 5.57798 7.80195 5.91109V12.809L7.80173 12.8088ZM12 15.2349L9.20509 13.6651V10.3351L12 8.76534L14.7947 10.3351V13.6651L12 15.2349ZM13.7958 22.4659C12.7967 22.4659 11.8693 22.1328 11.0841 21.5382L15.7699 18.8265C16.0553 18.6601 16.198 18.4222 16.198 18.0891V11.1912L18.1723 12.3329C18.3388 12.4279 18.4102 12.5707 18.4102 12.761V17.9939C18.4102 20.5389 16.4359 22.4657 13.7958 22.4657V22.4659ZM8.15848 17.1617L3.61528 14.5452C2.30696 13.784 1.42701 12.1667 1.42701 10.5969C1.42701 8.76534 2.52115 7.12414 4.20987 6.43428V11.8574C4.20987 12.1905 4.35266 12.4284 4.63802 12.5948L10.5846 16.0436L8.63415 17.1617C8.46771 17.2567 8.32492 17.2567 8.15848 17.1617ZM7.897 21.0625C5.20919 21.0625 3.23488 19.0407 3.23488 16.5432C3.23488 16.3529 3.25875 16.1626 3.2824 15.9723L7.96817 18.6839C8.25352 18.8504 8.53911 18.8504 8.82446 18.6839L14.7947 15.2351V17.4948C14.7947 17.6851 14.7233 17.8279 14.5568 17.9229L10.0136 20.5393C9.39518 20.8961 8.6578 21.0625 7.89677 21.0625H7.897ZM13.7958 23.8929C16.6739 23.8929 19.0762 21.8474 19.6235 19.1357C22.2874 18.4459 24 15.9484 24 13.4034C24 11.7383 23.2865 10.121 22.002 8.95542C22.121 8.45588 22.1924 7.95633 22.1924 7.45702C22.1924 4.0557 19.4331 1.51045 16.2458 1.51045C15.6037 1.51045 14.9852 1.60549 14.3668 1.81968C13.2963 0.773071 11.8215 0.107086 10.2042 0.107086C7.32606 0.107086 4.92383 2.15256 4.37653 4.86425C1.7126 5.55411 0 8.05161 0 10.5966C0 12.2617 0.713506 13.879 1.99795 15.0446C1.87904 15.5441 1.80764 16.0436 1.80764 16.543C1.80764 19.9443 4.56685 22.4895 7.75421 22.4895C8.39632 22.4895 9.01478 22.3945 9.63324 22.1803C10.7035 23.2269 12.1783 23.8929 13.7958 23.8929Z";

fn mark_path(mark: Mark) -> &'static str {
    match mark {
        Mark::Claude => CLAUDE_MARK_PATH,
        Mark::Codex => CODEX_MARK_PATH,
    }
}

/// Gauge geometry, in icon pixels (the 48×48 canvas is shown at 24×24pt, so
/// one unit here is one physical pixel on a Retina display).
///
/// Modelled on the iOS lock-screen "accessory circular" gauge: an open arc
/// with its gap at the bottom and the thing being measured in the middle.
/// There is no number in the icon — the arc is the reading, and the popover
/// has the exact figure one click away.
/// Centred a little low: the arc is open at the bottom, so a geometrically
/// centred ring sat 1.3px from the top edge but ~6px from the bottom — top
/// heavy, and the top got clipped in the bar. Lowered and trimmed slightly
/// so both margins are about 4px.
const GAUGE_CENTER: (f32, f32) = (24.0, 26.0);
const GAUGE_RADIUS: f32 = 19.0;
const GAUGE_STROKE: f32 = 6.4;
/// Where the arc starts and how far it goes, clockwise from east with y
/// down (so 90° is straight down). 130° is lower-left; a 280° sweep ends at
/// lower-right, leaving an 80° gap at the bottom — the ring deliberately does
/// not close under the logo. (Was 240°, with room for a number that is no
/// longer drawn; lengthened once that room was not needed.)
const ARC_START: f32 = 130.0;
const ARC_SWEEP: f32 = 280.0;
/// The unfilled part of the arc — still visible, just faint, so the reader
/// always sees the whole dial rather than a fill that appears to run out of
/// room to grow. Also the opacity of the logo when there is no reading.
const TRACK_ALPHA: f32 = 0.3;
/// The logo, centred in the ring with the room the number used to take.
const MARK_CENTER: (f32, f32) = GAUGE_CENTER;
const MARK_DIAMETER: f32 = 20.0;

/// Draw one provider's gauge: an open arc showing `percent` remaining (a
/// template image — macOS tints it white or black to match the bar) with the
/// provider's logo inside it. `None` means nothing is readable for this
/// provider: the arc stays at its faint track and the logo is drawn faint
/// too, so "we do not know" never reads as a solid, drained "0%".
pub fn render(percent: Option<u8>, mark: Mark) -> (Vec<u8>, u32, u32) {
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    let (cx, cy) = GAUGE_CENTER;
    let mut canvas = Canvas::new(w, h);

    let r_inner = GAUGE_RADIUS - GAUGE_STROKE / 2.0;
    let r_outer = GAUGE_RADIUS + GAUGE_STROKE / 2.0;

    // Round caps: a disc of the stroke's width centred on the arc at each
    // end. The wedge itself has flat radial cuts; the discs round them off.
    let cap = |canvas: &mut Canvas, angle_deg: f32, alpha: f32| {
        let a = angle_deg.to_radians();
        canvas.disc(cx + GAUGE_RADIUS * a.cos(), cy + GAUGE_RADIUS * a.sin(), GAUGE_STROKE / 2.0, alpha);
    };

    canvas.annular_wedge(cx, cy, r_inner, r_outer, ARC_START, ARC_START + ARC_SWEEP, TRACK_ALPHA);
    cap(&mut canvas, ARC_START, TRACK_ALPHA);
    cap(&mut canvas, ARC_START + ARC_SWEEP, TRACK_ALPHA);

    let mark_alpha = match percent {
        Some(p) => {
            let p = p.min(100);
            if p > 0 {
                let sweep = ARC_SWEEP * (p as f32 / 100.0);
                canvas.annular_wedge(cx, cy, r_inner, r_outer, ARC_START, ARC_START + sweep, 1.0);
                cap(&mut canvas, ARC_START, 1.0);
                cap(&mut canvas, ARC_START + sweep, 1.0);
            }
            1.0
        }
        None => TRACK_ALPHA,
    };

    let mut glyph = parse_svg_path(mark_path(mark));
    fit_path(&mut glyph, MARK_CENTER, MARK_DIAMETER);
    canvas.fill_path(&glyph, mark_alpha);

    let alpha = canvas.resolve(w, h);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for a in alpha {
        rgba.extend_from_slice(&[INK[0], INK[1], INK[2], (a * 255.0).round() as u8]);
    }
    (rgba, WIDTH, HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: [Option<u8>; 7] = [
        None,
        Some(0),
        Some(1),
        Some(20),
        Some(50),
        Some(99),
        Some(100),
    ];

    fn render(percent: Option<u8>) -> (Vec<u8>, u32, u32) {
        super::render(percent, Mark::Claude)
    }

    #[test]
    fn every_reading_renders_a_full_icon() {
        for percent in SAMPLE {
            let (px, w, h) = render(percent);
            assert_eq!((w, h), (WIDTH, HEIGHT));
            assert_eq!(px.len(), (w * h * 4) as usize);
        }
    }

    /// Template artwork carries no colour; a stray tint would render as a
    /// smudge that ignores the menu bar.
    #[test]
    fn the_artwork_is_pure_template_ink() {
        for percent in SAMPLE {
            let coloured = render(percent)
                .0
                .chunks(4)
                .filter(|p| p[3] > 0 && [p[0], p[1], p[2]] != INK)
                .count();
            assert_eq!(coloured, 0, "{percent:?} carries colour of its own");
        }
    }

    /// Both marks actually parse into something with area — a typo in either
    /// path constant should fail loudly, not silently render an empty icon.
    #[test]
    fn both_marks_parse_into_a_nonempty_shape() {
        for mark in [Mark::Claude, Mark::Codex] {
            let glyph = parse_svg_path(mark_path(mark));
            assert!(!glyph.is_empty(), "{mark:?} produced no subpaths");

            let (px, ..) = super::render(Some(50), mark);
            let opaque_px = px.chunks(4).filter(|p| p[3] > 200).count();
            assert!(opaque_px > 5, "{mark:?} drew almost no glyph ink ({opaque_px} px)");
        }
    }

    /// The two marks must not rasterise identically — otherwise the whole
    /// point (telling providers apart by logo) fails silently.
    #[test]
    fn the_two_marks_look_different() {
        let claude = super::render(Some(50), Mark::Claude).0;
        let codex = super::render(Some(50), Mark::Codex).0;
        let differing = claude.iter().zip(&codex).filter(|(a, b)| a != b).count();
        assert!(differing > 50, "Claude and Codex marks render alike ({differing} bytes differ)");
    }

    /// The *canvas* keeps its footprint at every reading — this is the
    /// property that actually matters, because it is what stops the menu bar
    /// from shuffling neighbouring icons as the reading changes. macOS lays
    /// out the status item from the image buffer's declared size, not from
    /// where the ink happens to be, and `render` always returns
    /// `WIDTH`×`HEIGHT` regardless of reading.
    #[test]
    fn the_canvas_size_never_changes() {
        for percent in SAMPLE {
            let (_, w, h) = render(percent);
            assert_eq!((w, h), (WIDTH, HEIGHT), "{percent:?} changed canvas size");
        }
    }

    /// A drained-but-known gauge (solid logo, empty arc) and an unreadable
    /// one (faint logo) must not look the same — "no headroom" and "no reading"
    /// are different situations.
    #[test]
    fn unknown_is_distinguishable_from_zero() {
        let alpha = |p| -> Vec<u8> { render(p).0.chunks(4).map(|px| px[3]).collect() };
        let differing = alpha(None)
            .iter()
            .zip(alpha(Some(0)))
            .filter(|(a, b)| **a != *b)
            .count();
        assert!(differing > 20, "unknown and 0% look alike ({differing} px)");
    }

    /// Every sample percentage must produce a visibly different icon from
    /// every other — otherwise the ring's sweep is not actually carrying
    /// information. The exact percentage itself lives in the status item's
    /// title text now, not this bitmap, but the ring alone still has to move.
    #[test]
    fn distinct_percentages_look_different() {
        let alpha = |p| -> Vec<u8> { render(p).0.chunks(4).map(|px| px[3]).collect() };
        let readings = [Some(0), Some(1), Some(20), Some(50), Some(99), Some(100)];
        for (i, &a) in readings.iter().enumerate() {
            for &b in readings.iter().skip(i + 1) {
                let differing = alpha(a)
                    .iter()
                    .zip(alpha(b))
                    .filter(|(x, y)| **x != *y)
                    .count();
                assert!(differing > 0, "{a:?} and {b:?} render identically");
            }
        }
    }

    /// A full ring (100%) and an empty one (0%) are the extremes, and the
    /// difference between them has to be substantial, not a rounding
    /// artefact.
    #[test]
    fn full_and_empty_differ_substantially() {
        let alpha = |p| -> Vec<u8> { render(p).0.chunks(4).map(|px| px[3]).collect() };
        let differing = alpha(Some(100))
            .iter()
            .zip(alpha(Some(0)))
            .filter(|(a, b)| **a != *b)
            .count();
        assert!(differing > 200, "full and empty barely differ ({differing} px)");
    }

    /// macOS gives a status-bar button 22pt of height and draws a taller image
    /// unscaled and centred (`NSImageScaleNone`, measured on a 30pt-tall
    /// menu bar) — so of this 24pt / 48px image, 1pt (2px) is cut off the top
    /// and the bottom. That crop is what clipped the top of the ring when it
    /// sat 1.3px from the edge. Nothing may be drawn in those rows.
    #[test]
    fn nothing_is_lost_to_the_status_bar_crop() {
        const BUTTON_PX: u32 = 44; // 22pt at 2x
        let crop = (HEIGHT - BUTTON_PX) / 2;
        for mark in [Mark::Claude, Mark::Codex] {
            for percent in SAMPLE {
                let (px, w, h) = super::render(percent, mark);
                for y in (0..crop).chain(h - crop..h) {
                    for x in 0..w {
                        assert_eq!(
                            px[((y * w + x) * 4 + 3) as usize],
                            0,
                            "{mark:?} {percent:?} draws in cropped row {y}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn nothing_touches_the_edges() {
        for percent in SAMPLE {
            let (px, w, h) = render(percent);
            let solid = |x: usize, y: usize| px[(y * w as usize + x) * 4 + 3] > 0;
            for x in 0..w as usize {
                assert!(!solid(x, 0) && !solid(x, h as usize - 1), "{percent:?}");
            }
            for y in 0..h as usize {
                assert!(!solid(0, y) && !solid(w as usize - 1, y), "{percent:?}");
            }
        }
    }

    /// Write a contact sheet to `AI_REACTOR_SHEET` as a PPM. Not a test — a 22pt
    /// icon is hard to judge, and this scales the readings up side by side on
    /// a light and a dark band, standing in for what macOS will tint them to.
    ///
    /// `AI_REACTOR_SHEET=/tmp/g.ppm cargo test write_level_sheet -- --ignored`
    #[test]
    #[ignore = "visual aid, not an assertion"]
    fn write_level_sheet() {
        let Ok(path) = std::env::var("AI_REACTOR_SHEET") else {
            return;
        };
        const SCALE: usize = 5;
        const PAD: usize = 8;

        let rows: Vec<(Option<u8>, Mark)> = SAMPLE
            .iter()
            .map(|&p| (p, Mark::Claude))
            .chain(SAMPLE.iter().map(|&p| (p, Mark::Codex)))
            .collect();

        let cell_w = WIDTH as usize * SCALE + PAD * 2;
        let cell_h = HEIGHT as usize * SCALE + PAD * 2;
        let (sheet_w, sheet_h) = (cell_w * 2, cell_h * rows.len());

        let mut buf = vec![0u8; sheet_w * sheet_h * 3];
        for (i, px) in buf.iter_mut().enumerate() {
            let x = (i / 3) % sheet_w;
            *px = if x < cell_w { 238 } else { 40 };
        }

        for (row, &(percent, mark)) in rows.iter().enumerate() {
            let (px, w, h) = super::render(percent, mark);
            for band in 0..2 {
                let ink = if band == 0 { 20.0 } else { 240.0 };
                for y in 0..h as usize * SCALE {
                    for x in 0..w as usize * SCALE {
                        let src = ((y / SCALE) * w as usize + (x / SCALE)) * 4;
                        let a = px[src + 3] as f32 / 255.0;
                        let ox = band * cell_w + PAD + x;
                        let out = ((row * cell_h + PAD + y) * sheet_w + ox) * 3;
                        for ch in 0..3 {
                            let bg = buf[out + ch] as f32;
                            buf[out + ch] = (ink * a + bg * (1.0 - a)) as u8;
                        }
                    }
                }
            }
        }

        use std::io::Write;
        let mut file = std::fs::File::create(&path).expect("create sheet");
        write!(file, "P6\n{sheet_w} {sheet_h}\n255\n").expect("header");
        file.write_all(&buf).expect("pixels");
        println!("wrote {path} ({sheet_w}x{sheet_h})");
    }

    /// Write the README's menu bar preview to `AI_REACTOR_PREVIEW` as a PPM:
    /// Claude at 72% and Codex at 38%, side by side on a dark menu bar, 3×.
    ///
    /// `AI_REACTOR_PREVIEW=/tmp/p.ppm cargo test write_readme_preview -- --ignored`
    #[test]
    #[ignore = "visual aid, not an assertion"]
    fn write_readme_preview() {
        let Ok(path) = std::env::var("AI_REACTOR_PREVIEW") else {
            return;
        };
        const SCALE: usize = 3;
        const PAD: usize = 12;
        const GAP: usize = 6;
        let icons = [
            super::render(Some(72), Mark::Claude),
            super::render(Some(38), Mark::Codex),
        ];
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        let sheet_w = (PAD * 2 + w * 2 + GAP) * SCALE;
        let sheet_h = (PAD * 2 + h) * SCALE;
        let mut buf = vec![0u8; sheet_w * sheet_h * 3];
        for px in buf.chunks_mut(3) {
            px.copy_from_slice(&[38, 38, 42]);
        }
        for (i, (px, ..)) in icons.iter().enumerate() {
            let ox = PAD + i * (w + GAP);
            for y in 0..h * SCALE {
                for x in 0..w * SCALE {
                    let a = px[((y / SCALE) * w + x / SCALE) * 4 + 3] as f32 / 255.0;
                    let out = ((PAD * SCALE + y) * sheet_w + ox * SCALE + x) * 3;
                    for ch in 0..3 {
                        let bg = buf[out + ch] as f32;
                        buf[out + ch] = (245.0 * a + bg * (1.0 - a)) as u8;
                    }
                }
            }
        }
        use std::io::Write;
        let mut file = std::fs::File::create(&path).expect("create preview");
        write!(file, "P6\n{sheet_w} {sheet_h}\n255\n").expect("header");
        file.write_all(&buf).expect("pixels");
    }

    /// Print every reading as ASCII. Not a test — a way to look at the
    /// artwork without building the app and squinting at a 22pt icon.
    ///
    /// `cargo test preview_levels -- --ignored --nocapture`
    #[test]
    #[ignore = "visual aid, not an assertion"]
    fn preview_levels() {
        const RAMP: [char; 5] = [' ', '.', ':', '#', '@'];
        for percent in SAMPLE {
            println!("\n=== {percent:?} ===");
            let (px, w, h) = render(percent);
            for y in 0..h as usize {
                let row: String = (0..w as usize)
                    .map(|x| {
                        let a = px[(y * w as usize + x) * 4 + 3] as usize;
                        RAMP[(a * (RAMP.len() - 1)) / 255]
                    })
                    .collect();
                if !row.trim().is_empty() {
                    println!("{row}");
                }
            }
        }
    }
}
