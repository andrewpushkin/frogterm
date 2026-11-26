//! The emulator window (Windows and Linux): the 800×480 panel in its six inks
//! and, under it, the reTerminal's three buttons to click, the LED and what
//! the device is doing.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::time::Instant;

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

/// Reference time for the LED blinking.
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Height of the button bar under the panel.
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
    /// The device is awake: presses only chirp.
    busy: AtomicBool,
    /// The status LED: 0 off, 1 on, 2 blinking.
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

    /// Whether the LED is lit now (blinking: 4 times a second).
    fn led_lit(&self) -> bool {
        match self.led.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => START.get_or_init(Instant::now).elapsed().as_millis() / 250 % 2 == 0,
        }
    }

    /// The device went to deep sleep until `at` (or a button press).
    pub fn sleeping(&self, at: Instant) {
        *self.wake_at.lock().unwrap() = Some(at);
        self.busy.store(false, Ordering::Relaxed);
    }
}

/// The three buttons, left to right.
const BUTTONS: [(Press, &str); 3] = [(Press::Previous, "< Previous"), (Press::Update, "Update"), (Press::Next, "Next >")];

/// Screen area of button `i`.
fn button_rect(i: usize) -> Rectangle {
    const BW: u32 = 180;
    const GAP: u32 = 24;
    let left = (WIDTH - 3 * BW - 2 * GAP) / 2;
    Rectangle::new(Point::new((left + i as u32 * (BW + GAP)) as i32, HEIGHT as i32 + 10), Size::new(BW, 40))
}

/// The button under window pixel (`x`, `y`).
fn button_at(x: f32, y: f32) -> Option<Press> {
    let p = Point::new(x as i32, y as i32);
    (0..BUTTONS.len()).find(|&i| button_rect(i).contains(p)).map(|i| BUTTONS[i].0)
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
    let mut panel = vec![0x00ff_ffffu32; W * HEIGHT as usize];
    let mut was_down = false;
    let mut held: Option<Press> = None;
    while window.is_open() && !window.is_key_down(Key::Escape) {
        if let Some(fb) = ui.screen.lock().unwrap().take() {
            for (i, px) in panel.iter_mut().enumerate() {
                let c = fb.get((i % W) as u32, (i / W) as u32).rgb();
                *px = u32::from(c.r()) << 16 | u32::from(c.g()) << 8 | u32::from(c.b());
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
                // as on the device: a chirp, the running update goes on
                log::info!("button pressed during the update: ignored (chirp)");
            } else {
                ui.set_busy(true);
                presses.send(p).map_err(|_| anyhow!("the device thread stopped"))?;
            }
        }

        pixels[..panel.len()].copy_from_slice(&panel);
        let wake_in = ui.wake_at.lock().unwrap().map(|at| at.saturating_duration_since(Instant::now()).as_secs());
        draw_bar(&mut pixels, ui.busy.load(Ordering::Relaxed), ui.led_lit(), held, wake_in, &crate::logger::last());
        window.update_with_buffer(&pixels, W, H).map_err(|e| anyhow!("window: {e}"))?;
    }
    Ok(())
}

/// Buttons, LED and status line under the panel.
fn draw_bar(pixels: &mut [u32], awake: bool, led_lit: bool, held: Option<Press>, wake_in: Option<u64>, last_log: &str) {
    let mut c = Canvas(pixels);
    let bar = Rectangle::new(Point::new(0, HEIGHT as i32), Size::new(WIDTH, BAR));
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
    // blinking while working, on while the panel rests, off when ready
    let led = if led_lit { Rgb888::new(255, 70, 50) } else { Rgb888::new(90, 40, 40) };
    let y = HEIGHT as i32 + 66;
    let _ = Circle::new(Point::new(12, y - 6), 12).into_styled(PrimitiveStyle::with_fill(led)).draw(&mut c);
    let state = match (awake, wake_in) {
        (true, _) => "awake".to_string(),
        (false, Some(s)) => format!("asleep, timer wake in {}", duration(s)),
        (false, None) => "asleep".to_string(),
    };
    let status: String = format!("{state}  |  {last_log}").chars().map(|ch| if (ch as u32) < 0x100 { ch } else { '?' }).collect();
    let _ = STATUS_FONT.render_aligned(
        status.as_str(),
        Point::new(32, y),
        VerticalPosition::Center,
        HorizontalAlignment::Left,
        FontColor::Transparent(Rgb888::new(230, 230, 230)),
        &mut c,
    );
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
                self.0[p.y as usize * W + p.x as usize] = u32::from(c.r()) << 16 | u32::from(c.g()) << 8 | u32::from(c.b());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_sit_side_by_side_under_the_panel() {
        let rects: Vec<Rectangle> = (0..3).map(button_rect).collect();
        for r in &rects {
            assert!(r.top_left.y >= HEIGHT as i32, "below the panel");
            assert!(r.bottom_right().unwrap().y < H as i32 && r.bottom_right().unwrap().x < W as i32);
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
        assert_eq!(hit(at(0)), Some(Press::Previous));
        assert_eq!(hit(at(1)), Some(Press::Update));
        assert_eq!(hit(at(2)), Some(Press::Next));
        assert_eq!(hit(Point::new(400, 200)), None, "on the panel");
        assert_eq!(hit(Point::new(2, HEIGHT as i32 + 20)), None, "beside the buttons");
    }

    #[test]
    fn durations_read_well() {
        assert_eq!(duration(40), "40 s");
        assert_eq!(duration(185), "3 min 5 s");
        assert_eq!(duration(7 * 3600 + 12 * 60 + 9), "7 h 12 min");
    }

    #[test]
    fn bar_draws_inside_the_window() {
        let mut px = vec![0u32; W * H];
        draw_bar(&mut px, false, true, Some(Press::Update), Some(3600), "sleeping until Thu 26 Nov 06:00 local, °C ♪ Шалом");
        assert!(px[..W * HEIGHT as usize].iter().all(|&p| p == 0), "the panel area is left alone");
        let c = button_rect(1).top_left + Point::new(20, 5);
        assert_ne!(px[c.y as usize * W + c.x as usize], 0, "the green button is drawn");
    }
}
