//! The emulator window (Windows and Linux): on top, as on the device, the
//! reTerminal's three buttons to click, the green LED and what the device is
//! doing; under them the 800×480 panel in its six inks.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, PrimitiveStyle, Rectangle, RoundedRectangle};
use frogterm_core::app::Led;
use frogterm_core::buttons::Press;
use frogterm_core::framebuf::{FrameBuffer, HEIGHT, WIDTH};
use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use u8g2_fonts::{fonts, FontRenderer};

/// How long the real panel takes to show a new image; the window fades it in
/// over the same time.
const REFRESH: Duration = Duration::from_secs(20);

/// Height of the button bar above the panel.
const BAR: u32 = 84;
const W: usize = WIDTH as usize;
const H: usize = (HEIGHT + BAR) as usize;

static BUTTON_FONT: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvB12_tf>();
static STATUS_FONT: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvR10_tf>();

/// What the device thread tells the window.
#[derive(Default)]
pub struct Shared {
    /// A new panel image, not drawn yet.
    screen: Mutex<Option<FrameBuffer>>,
    /// The device is awake: presses get the busy beep and are ignored.
    busy: AtomicBool,
    /// The status LED: 0 off (ready), 1 on (busy).
    led: AtomicU8,
    /// When the timer wakes the sleeping device.
    wake_at: Mutex<Option<Instant>>,
}

impl Shared {
    pub fn show(&self, fb: FrameBuffer) {
        *self.screen.lock().unwrap() = Some(fb);
    }

    pub fn set_busy(&self, busy: bool) {
        self.busy.store(busy, Ordering::Relaxed);
        if busy {
            *self.wake_at.lock().unwrap() = None;
        }
    }

    pub fn set_led(&self, led: Led) {
        self.led.store(led as u8, Ordering::Relaxed);
    }

    /// Whether the LED is lit now.
    fn led_lit(&self) -> bool {
        self.led.load(Ordering::Relaxed) != 0
    }

    /// The device went to deep sleep until `at` (or a button press).
    pub fn sleeping(&self, at: Instant) {
        *self.wake_at.lock().unwrap() = Some(at);
        self.busy.store(false, Ordering::Relaxed);
    }
}

/// The three buttons on the top edge, left to right (as on the device).
const BUTTONS: [(Press, &str); 3] = [(Press::Previous, "< Previous"), (Press::Next, "Next >"), (Press::Update, "Update")];

/// Screen area of button `i`.
fn button_rect(i: usize) -> Rectangle {
    const BW: u32 = 180;
    const GAP: u32 = 24;
    let left = (WIDTH - 3 * BW - 2 * GAP) / 2;
    Rectangle::new(Point::new((left + i as u32 * (BW + GAP)) as i32, 10), Size::new(BW, 40))
}

/// The button under window pixel (`x`, `y`).
fn button_at(x: f32, y: f32) -> Option<Press> {
    let p = Point::new(x as i32, y as i32);
    (0..BUTTONS.len()).find(|&i| button_rect(i).contains(p)).map(|i| BUTTONS[i].0)
}

/// The time left as a clock: "6:37:12", "3:05", "0:40".
pub fn countdown(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// "7 h 12 min", "3 min 5 s", "40 s".
pub fn duration(secs: u64) -> String {
    match secs {
        s if s >= 3600 => format!("{} h {} min", s / 3600, s % 3600 / 60),
        s if s >= 60 => format!("{} min {} s", s / 60, s % 60),
        s => format!("{s} s"),
    }
}

/// Runs the window until it is closed; clicks and keys go to `presses`.
pub fn run(ui: &Shared, presses: Sender<Press>) -> Result<()> {
    let mut window = Window::new("frogterm - reTerminal E1002 emulator", W, H, WindowOptions::default())
        .map_err(|e| anyhow!("cannot open a window ({e}); without a display run `make emulator ARGS=--once`"))?;
    window.set_target_fps(30);
    let mut pixels = vec![0u32; W * H];
    // what the window shows, fading from `old` to `new` since `fade_start`
    let mut panel = vec![0x00ff_ffffu32; W * HEIGHT as usize];
    let mut old = panel.clone();
    let mut new = panel.clone();
    let mut fade_start: Option<Instant> = None;
    let mut was_down = false;
    let mut held: Option<Press> = None;
    while window.is_open() && !window.is_key_down(Key::Escape) {
        if let Some(fb) = ui.screen.lock().unwrap().take() {
            old.copy_from_slice(&panel); // also from the middle of a fade
            panel_pixels(&fb, &mut new);
            fade_start = Some(Instant::now());
        }
        if let Some(start) = fade_start {
            let t = start.elapsed().as_secs_f32() / REFRESH.as_secs_f32();
            blend(&old, &new, t, &mut panel);
            if t >= 1.0 {
                fade_start = None;
            }
        }

        // a press is a click on a button, or a key
        let mut pressed = Vec::new();
        let mouse = window.get_mouse_pos(MouseMode::Discard);
        let down = window.get_mouse_down(MouseButton::Left);
        if down && !was_down {
            held = mouse.and_then(|(x, y)| button_at(x, y));
            pressed.extend(held);
        }
        if !down {
            held = None;
        }
        was_down = down;
        for key in window.get_keys_pressed(KeyRepeat::No) {
            pressed.extend(match key {
                Key::Left => Some(Press::Previous),
                Key::Right => Some(Press::Next),
                Key::Space | Key::Enter | Key::G => Some(Press::Update),
                _ => None,
            });
        }
        let busy = ui.busy.load(Ordering::Relaxed);
        for p in pressed {
            if busy {
                // the device's rule: ignored with the busy pips, the wake goes on
                crate::emulator::play(frogterm_core::app::press_while_awake(p));
            } else {
                ui.set_busy(true);
                presses.send(p).map_err(|_| anyhow!("the device thread stopped"))?;
            }
        }

        draw_window(&mut pixels, &panel, ui, held);
        window.update_with_buffer(&pixels, W, H).map_err(|e| anyhow!("window: {e}"))?;
    }
    Ok(())
}

/// The window as an image (bar and the last panel image), as `--once` saves it.
pub fn snapshot(ui: &Shared) -> Option<image::RgbImage> {
    let fb = ui.screen.lock().unwrap().take()?;
    let mut panel = vec![0u32; W * HEIGHT as usize];
    panel_pixels(&fb, &mut panel);
    let mut pixels = vec![0u32; W * H];
    draw_window(&mut pixels, &panel, ui, None);
    Some(image::RgbImage::from_fn(W as u32, H as u32, |x, y| {
        let p = pixels[y as usize * W + x as usize];
        image::Rgb([(p >> 16) as u8, (p >> 8) as u8, p as u8])
    }))
}

/// `out` = `from` turned into `to` by `t` (0..=1, eased: slow start and end).
fn blend(from: &[u32], to: &[u32], t: f32, out: &mut [u32]) {
    let t = t.clamp(0.0, 1.0);
    let k = (t * t * (3.0 - 2.0 * t) * 256.0) as u32; // smoothstep, 0..=256
    for ((o, &a), &b) in out.iter_mut().zip(from).zip(to) {
        let ch = |shift: u32| {
            let (a, b) = ((a >> shift) & 0xff, (b >> shift) & 0xff);
            ((a * (256 - k) + b * k) >> 8) << shift
        };
        *o = ch(16) | ch(8) | ch(0);
    }
}

fn panel_pixels(fb: &FrameBuffer, panel: &mut [u32]) {
    for (i, px) in panel.iter_mut().enumerate() {
        *px = rgb32(fb.get((i % W) as u32, (i / W) as u32).rgb());
    }
}

/// Bar on top, the panel under it.
fn draw_window(pixels: &mut [u32], panel: &[u32], ui: &Shared, held: Option<Press>) {
    pixels[W * BAR as usize..].copy_from_slice(panel);
    let wake_in = ui.wake_at.lock().unwrap().map(|at| at.saturating_duration_since(Instant::now()).as_secs());
    draw_bar(pixels, ui.busy.load(Ordering::Relaxed), ui.led_lit(), held, wake_in, &crate::logger::last());
}

/// Buttons, LED and status line above the panel.
fn draw_bar(pixels: &mut [u32], awake: bool, led_lit: bool, held: Option<Press>, wake_in: Option<u64>, last_log: &str) {
    let mut c = Canvas(pixels);
    let bar = Rectangle::new(Point::zero(), Size::new(WIDTH, BAR));
    let _ = bar.into_styled(PrimitiveStyle::with_fill(Rgb888::new(52, 54, 58))).draw(&mut c);
    for (i, &(press, label)) in BUTTONS.iter().enumerate() {
        let (face, text) = match press {
            Press::Update => (Rgb888::new(40, 160, 70), Rgb888::WHITE),
            _ => (Rgb888::new(205, 208, 212), Rgb888::new(30, 30, 30)),
        };
        let face = if held == Some(press) { dim(face) } else { face };
        let r = button_rect(i);
        let _ = RoundedRectangle::with_equal_corners(r, Size::new(8, 8)).into_styled(PrimitiveStyle::with_fill(face)).draw(&mut c);
        let _ = BUTTON_FONT.render_aligned(
            label,
            r.center(),
            VerticalPosition::Center,
            HorizontalAlignment::Center,
            FontColor::Transparent(if awake { dim(text) } else { text }),
            &mut c,
        );
    }
    // on while busy (working, or the panel rests), off when ready
    let led = if led_lit { Rgb888::new(60, 230, 90) } else { Rgb888::new(35, 75, 45) };
    let y = 66;
    let _ = Circle::new(Point::new(12, y - 6), 12).into_styled(PrimitiveStyle::with_fill(led)).draw(&mut c);
    // right: the countdown to the timer wake, ticking every second
    let mut status_end = WIDTH as i32 - 12;
    if let (false, Some(secs)) = (awake, wake_in) {
        let text = format!("timer wake in {}", countdown(secs));
        let right = Point::new(WIDTH as i32 - 12, y);
        if let Ok(d) = BUTTON_FONT.render_aligned(
            text.as_str(),
            right,
            VerticalPosition::Center,
            HorizontalAlignment::Right,
            FontColor::Transparent(Rgb888::new(255, 255, 255)),
            &mut c,
        ) {
            status_end = d.map_or(status_end, |b| b.top_left.x) - 16;
        }
    }
    let state = if awake { "awake" } else { "asleep" };
    let status: String = format!("{state}  |  {last_log}").chars().map(|ch| if (ch as u32) < 0x100 { ch } else { '?' }).collect();
    let status = fit(&STATUS_FONT, &status, status_end - 32);
    let _ = STATUS_FONT.render_aligned(
        status.as_str(),
        Point::new(32, y),
        VerticalPosition::Center,
        HorizontalAlignment::Left,
        FontColor::Transparent(Rgb888::new(230, 230, 230)),
        &mut c,
    );
}

/// `text`, cut with "..." to at most `width` pixels in `font`.
fn fit(font: &FontRenderer, text: &str, width: i32) -> String {
    let w = |t: &str| font.get_rendered_dimensions(t, Point::zero(), VerticalPosition::Center).map_or(0, |d| d.advance.x);
    if w(text) <= width {
        return text.to_string();
    }
    let mut cut: String = text.to_string();
    while !cut.is_empty() && w(&format!("{cut}...")) > width {
        cut.pop();
    }
    format!("{cut}...")
}

fn rgb32(c: Rgb888) -> u32 {
    (u32::from(c.r()) << 16) | (u32::from(c.g()) << 8) | u32::from(c.b())
}

fn dim(c: Rgb888) -> Rgb888 {
    Rgb888::new(c.r() / 3 * 2, c.g() / 3 * 2, c.b() / 3 * 2)
}

/// The window's 0RGB pixels as an embedded-graphics target.
struct Canvas<'a>(&'a mut [u32]);

impl OriginDimensions for Canvas<'_> {
    fn size(&self) -> Size {
        Size::new(W as u32, H as u32)
    }
}

impl DrawTarget for Canvas<'_> {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb888>>>(&mut self, pixels: I) -> Result<(), Self::Error> {
        for Pixel(p, c) in pixels {
            if (0..W as i32).contains(&p.x) && (0..H as i32).contains(&p.y) {
                self.0[p.y as usize * W + p.x as usize] = rgb32(c);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_sit_side_by_side_above_the_panel() {
        let rects: Vec<Rectangle> = (0..3).map(button_rect).collect();
        for r in &rects {
            assert!(r.top_left.y >= 0 && r.bottom_right().unwrap().y < BAR as i32, "in the bar above the panel");
            assert!(r.bottom_right().unwrap().x < W as i32);
        }
        assert!(rects[0].bottom_right().unwrap().x < rects[1].top_left.x);
        assert!(rects[1].bottom_right().unwrap().x < rects[2].top_left.x);
        let right_margin = WIDTH as i32 - (rects[2].top_left.x + rects[2].size.width as i32);
        assert_eq!(rects[0].top_left.x, right_margin, "centred");
    }

    #[test]
    fn clicks_find_their_button() {
        let at = |i: usize| button_rect(i).center();
        let hit = |p: Point| button_at(p.x as f32, p.y as f32);
        assert_eq!(hit(at(0)), Some(Press::Previous), "left");
        assert_eq!(hit(at(1)), Some(Press::Next), "right");
        assert_eq!(hit(at(2)), Some(Press::Update), "green, last");
        assert_eq!(hit(Point::new(400, 300)), None, "on the panel");
        assert_eq!(hit(Point::new(2, 20)), None, "beside the buttons");
    }

    #[test]
    fn durations_read_well() {
        assert_eq!(duration(40), "40 s");
        assert_eq!(duration(185), "3 min 5 s");
        assert_eq!(duration(7 * 3600 + 12 * 60 + 9), "7 h 12 min");
    }

    #[test]
    fn new_image_fades_in_from_the_old_one() {
        let (from, to) = ([0x00ff_ffff, 0x0000_0000], [0x0000_0000, 0x00ff_8000]);
        let mut out = [0u32; 2];
        blend(&from, &to, 0.0, &mut out);
        assert_eq!(out, from, "starts with the old image");
        blend(&from, &to, 1.0, &mut out);
        assert_eq!(out, to, "ends with the new one");
        blend(&from, &to, 5.0, &mut out);
        assert_eq!(out, to, "and stays there");
        blend(&from, &to, 0.5, &mut out);
        assert_eq!(out, [0x007f_7f7f, 0x007f_4000], "half way: the colours mixed");
        blend(&from, &to, 0.1, &mut out);
        assert!(out[0] >> 16 > 0xf0, "slow start: still almost white ({:06x})", out[0]);
        assert_eq!(REFRESH, Duration::from_secs(20), "as long as the real panel");
    }

    #[test]
    fn countdown_ticks_like_a_clock() {
        assert_eq!(countdown(40), "0:40");
        assert_eq!(countdown(185), "3:05");
        assert_eq!(countdown(6 * 3600 + 37 * 60 + 12), "6:37:12");
        assert_eq!(countdown(36 * 3600), "36:00:00");
    }

    #[test]
    fn long_status_is_cut_to_fit() {
        let long = "x".repeat(500);
        let cut = fit(&STATUS_FONT, &long, 300);
        assert!(cut.ends_with("...") && cut.len() < long.len());
        let w = STATUS_FONT.get_rendered_dimensions(cut.as_str(), Point::zero(), VerticalPosition::Center).unwrap().advance.x;
        assert!(w <= 300, "{w} px");
        assert_eq!(fit(&STATUS_FONT, "short", 300), "short");
    }

    #[test]
    fn countdown_is_drawn_at_the_right_only_while_asleep() {
        let right_part = |px: &[u32]| (0..BAR as usize).any(|y| (W - 60..W - 12).any(|x| px[y * W + x] == 0x00ff_ffff));
        let mut asleep = vec![0u32; W * H];
        draw_bar(&mut asleep, false, false, None, Some(3725), "");
        assert!(right_part(&asleep), "white countdown digits at the right");
        let mut awake = vec![0u32; W * H];
        draw_bar(&mut awake, true, false, None, None, "");
        assert!(!right_part(&awake), "no countdown while awake");
    }

    #[test]
    fn bar_draws_inside_the_window() {
        let mut px = vec![0u32; W * H];
        draw_bar(&mut px, false, true, Some(Press::Update), Some(3600), "sleeping until Thu 26 Nov 06:00 local, °C ♪ Шалом");
        assert!(px[W * BAR as usize..].iter().all(|&p| p == 0), "the panel area is left alone");
        let c = button_rect(2).top_left + Point::new(20, 5);
        assert_eq!(px[c.y as usize * W + c.x as usize], rgb32(dim(Rgb888::new(40, 160, 70))), "the green button, held");
        let led = px[66 * W + 18];
        assert!((led >> 8 & 0xff) > (led >> 16 & 0xff), "the LED is green");
    }

    #[test]
    fn snapshot_has_the_bar_on_top_of_the_panel() {
        let ui = Shared::default();
        assert!(snapshot(&ui).is_none(), "nothing shown yet");
        ui.show(FrameBuffer::colour_bars());
        let img = snapshot(&ui).expect("a panel image");
        assert_eq!(img.dimensions(), (W as u32, H as u32));
        assert_eq!(img.get_pixel(1, 1).0, [52, 54, 58], "the bar's grey on top");
        let c = FrameBuffer::colour_bars().get(1, 1).rgb();
        assert_eq!(img.get_pixel(1, BAR + 1).0, [c.r(), c.g(), c.b()], "the panel under it");
    }
}
