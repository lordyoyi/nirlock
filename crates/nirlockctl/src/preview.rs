//! A live view of the IR camera, drawn in the terminal.
//!
//! Enrolment without a preview asks someone to trust that the camera can
//! see them while a text line says "I cannot see a face". Showing the feed
//! turns that into something they can fix: too close, off to one side, in
//! the dark. Face ID and Windows Hello both do this, and for the same
//! reason.
//!
//! It is drawn with half-block characters rather than in a window: no GUI
//! toolkit, no root window on Wayland (enrolment runs under sudo because
//! the template belongs to the daemon), and it works unchanged in the
//! floating terminal Omarchy's menu opens, and over SSH.
//!
//! Each cell is `▀` with the foreground set to the pixel above and the
//! background to the pixel below, so one character row shows two image
//! rows. At the default size that is 64x64 samples of a 640x360 frame —
//! coarse, but framing and lighting are exactly what coarse shows well.

use std::io::Write as _;

use nirlock_vision::image::GrayImage;

/// Half-block cells. Kept small so the whole view fits an 80x24 terminal.
pub const COLS: usize = 64;
pub const ROWS: usize = 16;

pub struct Screen {
    /// False when the terminal cannot show colour, in which case the caller
    /// falls back to plain text.
    pub graphical: bool,
}

impl Screen {
    pub fn probe() -> Self {
        let colour = std::env::var("COLORTERM")
            .map(|v| v.contains("truecolor") || v.contains("24bit"))
            .unwrap_or(false);
        let dumb = std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false);
        Self {
            graphical: colour && !dumb && std::io::IsTerminal::is_terminal(&std::io::stdout()),
        }
    }

    pub fn enter(&self) {
        if self.graphical {
            // Alternate screen + hidden cursor, so the preview does not
            // scroll the user's scrollback away and is gone afterwards.
            print!("\x1b[?1049h\x1b[?25l");
            let _ = std::io::stdout().flush();
        }
    }

    pub fn leave(&self) {
        if self.graphical {
            print!("\x1b[?25h\x1b[?1049l");
            let _ = std::io::stdout().flush();
        }
    }
}

/// Nearest-neighbour sample of `img` at a normalised position.
fn sample(img: &GrayImage, fx: f64, fy: f64) -> u8 {
    let x = ((fx * img.width() as f64) as usize).min(img.width().saturating_sub(1));
    let y = ((fy * img.height() as f64) as usize).min(img.height().saturating_sub(1));
    img.as_slice()[y * img.width() + x]
}

/// The camera is not mirrored, but people expect a mirror: moving right
/// should move the image right. Flipping here costs nothing and removes a
/// constant small confusion while someone is trying to position themselves.
fn mirrored(fx: f64) -> f64 {
    1.0 - fx
}

/// Renders one frame as half-block rows, with `box_` (x, y, w, h in image
/// pixels) drawn as a bracket around the detected face.
pub fn frame(img: &GrayImage, face: Option<(f32, f32, f32, f32)>) -> String {
    let mut out = String::with_capacity(COLS * ROWS * 24);
    let in_box = |px: f64, py: f64| -> bool {
        let Some((bx, by, bw, bh)) = face else {
            return false;
        };
        let (x, y) = (px * img.width() as f64, py * img.height() as f64);
        let (bx, by, bw, bh) = (bx as f64, by as f64, bw as f64, bh as f64);
        let on_v = (x - bx).abs() < 3.0 || (x - (bx + bw)).abs() < 3.0;
        let on_h = (y - by).abs() < 3.0 || (y - (by + bh)).abs() < 3.0;
        let inside_v = y >= by && y <= by + bh;
        let inside_h = x >= bx && x <= bx + bw;
        (on_v && inside_v) || (on_h && inside_h)
    };
    for r in 0..ROWS {
        for c in 0..COLS {
            let fx = mirrored((c as f64 + 0.5) / COLS as f64);
            let fy_top = (r as f64 * 2.0 + 0.5) / (ROWS * 2) as f64;
            let fy_bot = (r as f64 * 2.0 + 1.5) / (ROWS * 2) as f64;
            let (mut t, mut b) = (sample(img, fx, fy_top), sample(img, fx, fy_bot));
            // The IR frame is dim; a gamma lift makes the face readable
            // without pretending the exposure is better than it is.
            t = lift(t);
            b = lift(b);
            let (mut tr, mut tg, mut tb) = (t, t, t);
            let (mut br, mut bg, mut bb) = (b, b, b);
            if in_box(fx, fy_top) {
                tr = 90;
                tg = 220;
                tb = 120;
            }
            if in_box(fx, fy_bot) {
                br = 90;
                bg = 220;
                bb = 120;
            }
            out.push_str(&format!(
                "\x1b[38;2;{tr};{tg};{tb}m\x1b[48;2;{br};{bg};{bb}m▀"
            ));
        }
        out.push_str("\x1b[0m\r\n");
    }
    out
}

fn lift(v: u8) -> u8 {
    // v^(1/1.6), cheap and monotonic.
    let f = (v as f64 / 255.0).powf(1.0 / 1.6);
    (f * 255.0) as u8
}

/// Draws the whole enrolment view: title, the five areas laid out as they
/// sit on the screen, the preview, and one hint line.
pub fn draw(img: &GrayImage, face: Option<(f32, f32, f32, f32)>, done: &[bool], hint: &str) {
    let mark = |i: usize| {
        if done[i] {
            "\x1b[32m●\x1b[0m"
        } else {
            "\x1b[90m○\x1b[0m"
        }
    };
    let pad = " ".repeat(COLS / 2 - 6);
    let mut s = String::new();
    s.push_str("\x1b[H"); // home, no clear: redrawing over the same cells does not flicker
    s.push_str("\x1b[1m  nirlock — enrolling\x1b[0m\r\n\r\n");
    s.push_str(&format!("{pad}{} top of screen\r\n", mark(3)));
    s.push_str(&frame(img, face));
    s.push_str(&format!(
        "{} left edge{}{} right edge\r\n",
        mark(1),
        " ".repeat(COLS.saturating_sub(24)),
        mark(2)
    ));
    s.push_str(&format!("{pad}{} bottom of screen\r\n", mark(4)));
    s.push_str(&format!("\r\n  {} middle of screen\r\n", mark(0)));
    s.push_str(&format!("\r\n  \x1b[36m{hint:<60}\x1b[0m\r\n"));
    print!("{s}");
    let _ = std::io::stdout().flush();
}
