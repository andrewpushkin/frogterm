//! The two boxes drawn over the photo: today's events (left) and weather (right).
//! Mirrored, same size, along the bottom edge; together they cover < 20 % of
//! the screen (owner's limit).
//!
//! ```text
//! ┌──────────────────────────────┐ ┌──────────────────────────────┐
//! │Wed 25 Nov  Tel Aviv 06:00 │ │ 06   07   08   09   10   11  │
//! │08:00  Иврит / Язык (שפה)     │ │ ☼    ☼    ☁    ☁    ☂    ☂   │
//! │08:45  Математика (חשבון)     │ │ 14°  16°  18°  19°  18°  17° │
//! │13:00  Акробатика    +5 more  │ │ 0%   0%   10%  20%  60%  70% │
//! └──────────────────────────────┘ └──────────────────────────────┘
//! ```

use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{
    Circle, Line, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, StrokeAlignment, Triangle,
};
use u8g2_fonts::fonts;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};
use u8g2_fonts::FontRenderer;

use crate::calendar::DayEvent;
use crate::framebuf::{FrameBuffer, Ink, HEIGHT, WIDTH};
use crate::text::{draw_mixed, mixed_width};
use crate::weather::{HourSlot, Icon};

/// Icons are drawn inside an `ICON_SIZE`×`ICON_SIZE` square.
pub const ICON_SIZE: u32 = 28;
/// Event lines under the header; with more events the last line says "+N more".
pub const EVENT_ROWS: usize = 4;

pub const BOX_W: u32 = 388;
pub const BOX_H: u32 = 96;
const MARGIN: u32 = 8;
const PAD: i32 = 8;

/// How a box sits on the photo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backdrop {
    /// White box (the photo under it is hidden).
    Solid,
    /// No fill: the photo shows through (pre-lightened with `dither::veil`);
    /// text gets a white outline so it stays readable.
    Clear,
}

pub struct EventsInfo<'a> {
    /// e.g. "Wed 25 Nov".
    pub date: &'a str,
    /// Right side of the header, e.g. "Tel Aviv 06:00".
    pub status: &'a str,
    /// Room temperature from the device's sensor (small house icon).
    pub room_c: Option<f32>,
    /// Battery charge (small battery icon); red below 20 %.
    pub battery_pct: Option<u8>,
    /// `None` = calendar could not be fetched; empty = nothing (left) today.
    pub events: Option<&'a [DayEvent]>,
    /// `events` are tomorrow's: the header says "Tomorrow" instead of the date.
    pub tomorrow: bool,
    /// Sunrise and sunset (local), shown left of `status`.
    pub sun: Option<((u8, u8), (u8, u8))>,
}

static TITLE_FONT: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvB12_tf>();
static SMALL_FONT: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvB10_tf>();
static TEMP_FONT: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvB18_tf>();

// weather box rows (tops, relative to the box)
const COL_W: i32 = 62;
const HOUR_Y: i32 = 5;
const ICON_CY: i32 = 34;
const TEMP_Y: i32 = 50;
const RAIN_Y: i32 = 75;
// events box rows
const HEADER_Y: i32 = 5;
const RULE_Y: i32 = 22;
const ROW_Y: i32 = 26;
const ROW_H: i32 = 17;

/// From this UV index on (WHO: "high") the UV is shown when it does not rain.
pub const UV_HIGH: f32 = 6.0;

/// Bottom row of a weather column: the rain chance if above 0 %, else a high
/// UV index, else nothing.
pub fn rain_or_uv(slot: &HourSlot) -> Option<(String, Ink)> {
    if slot.precip_pct > 0 {
        Some((format!("{}%", slot.precip_pct), Ink::Blue))
    } else if slot.uv >= UV_HIGH {
        Some((format!("UV {}", slot.uv.round() as i32), Ink::Red))
    } else {
        None
    }
}

/// Weather column heading, e.g. "14:00".
pub fn hour_label(hour: u8) -> String {
    format!("{hour:02}:00")
}

/// Event time column: "13:00-14:00", "22:00-..." (ends after today) or "all day".
pub fn event_time_label(e: &DayEvent) -> String {
    match (e.start, e.end) {
        (None, _) => "all day".to_string(),
        (Some((h, m)), Some((eh, em))) => format!("{h:02}:{m:02}-{eh:02}:{em:02}"),
        (Some((h, m)), None) => format!("{h:02}:{m:02}-..."),
    }
}

/// Bottom-left box.
pub fn events_rect() -> Rectangle {
    Rectangle::new(Point::new(MARGIN as i32, (HEIGHT - MARGIN - BOX_H) as i32), Size::new(BOX_W, BOX_H))
}

/// Bottom-right box.
pub fn weather_rect() -> Rectangle {
    Rectangle::new(
        Point::new((WIDTH - MARGIN - BOX_W) as i32, (HEIGHT - MARGIN - BOX_H) as i32),
        Size::new(BOX_W, BOX_H),
    )
}

/// The box outline (and white fill for `Solid`); everything drawn through the
/// returned target is clipped to the box, so text can never spill onto the photo.
fn frame<'a>(
    fb: &'a mut FrameBuffer,
    r: &Rectangle,
    backdrop: Backdrop,
) -> embedded_graphics::draw_target::Clipped<'a, FrameBuffer> {
    let mut t = fb.clipped(r);
    let style = match backdrop {
        Backdrop::Solid => PrimitiveStyleBuilder::new()
            .fill_color(Ink::White)
            .stroke_color(Ink::Black)
            .stroke_width(2)
            .stroke_alignment(StrokeAlignment::Inside)
            .build(),
        Backdrop::Clear => PrimitiveStyleBuilder::new()
            .stroke_color(Ink::Black)
            .stroke_width(1)
            .stroke_alignment(StrokeAlignment::Inside)
            .build(),
    };
    let _ = r.into_styled(style).draw(&mut t);
    t
}

/// Draws labels; on a `Clear` backdrop every label first gets a 1 px white
/// outline so it reads on any photo.
#[derive(Clone, Copy)]
struct Pen {
    halo: bool,
}

const OUTLINE: [(i32, i32); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];

impl Pen {
    fn new(backdrop: Backdrop) -> Pen {
        Pen { halo: backdrop == Backdrop::Clear }
    }

    fn text<D: DrawTarget<Color = Ink>>(self, t: &mut D, font: &FontRenderer, s: &str, pos: Point, align: HorizontalAlignment, ink: Ink) {
        // a missing glyph or draw error only loses a label, never the whole screen
        if self.halo {
            for (dx, dy) in OUTLINE {
                let at = pos + Point::new(dx, dy);
                let _ = font.render_aligned(s, at, VerticalPosition::Top, align, FontColor::Transparent(Ink::White), t);
            }
        }
        let _ = font.render_aligned(s, pos, VerticalPosition::Top, align, FontColor::Transparent(ink), t);
    }

    fn mixed<D: DrawTarget<Color = Ink>>(self, t: &mut D, s: &str, pos: Point, ink: Ink) {
        if self.halo {
            for (dx, dy) in OUTLINE {
                draw_mixed(t, s, pos + Point::new(dx, dy), Ink::White);
            }
        }
        draw_mixed(t, s, pos, ink);
    }
}

/// `None` = weather could not be fetched.
pub fn draw_weather_box(fb: &mut FrameBuffer, forecast: Option<&[HourSlot]>, backdrop: Backdrop) {
    let pen = Pen::new(backdrop);
    let r = weather_rect();
    let o = r.top_left;
    let mut t = frame(fb, &r, backdrop);
    let Some(slots) = forecast else {
        let mid = o + Point::new((BOX_W / 2) as i32, 40);
        pen.text(&mut t, &TITLE_FONT, "weather unavailable", mid, HorizontalAlignment::Center, Ink::Black);
        return;
    };
    let left = (BOX_W as i32 - 6 * COL_W) / 2;
    for (i, s) in slots.iter().take(6).enumerate() {
        let cx = o.x + left + COL_W * i as i32 + COL_W / 2;
        let at = |y: i32| Point::new(cx, o.y + y);
        let c = HorizontalAlignment::Center;
        pen.text(&mut t, &SMALL_FONT, &hour_label(s.hour), at(HOUR_Y), c, Ink::Black);
        draw_icon(&mut t, s.icon, at(ICON_CY), (!s.is_day).then_some(s.moon));
        pen.text(&mut t, &TEMP_FONT, &format!("{}°", s.temp_c.round() as i32), at(TEMP_Y), c, Ink::Black);
        if let Some((label, ink)) = rain_or_uv(s) {
            pen.text(&mut t, &SMALL_FONT, &label, at(RAIN_Y), c, ink);
        }
    }
}

pub fn draw_events_box(fb: &mut FrameBuffer, info: &EventsInfo, backdrop: Backdrop) {
    let pen = Pen::new(backdrop);
    let r = events_rect();
    let o = r.top_left;
    let mut t = frame(fb, &r, backdrop);
    let at = |x: i32, y: i32| o + Point::new(x, y);
    let (day, day_ink) = if info.tomorrow { ("Tomorrow", Ink::Blue) } else { (info.date, Ink::Black) };
    pen.text(&mut t, &TITLE_FONT, day, at(PAD, HEADER_Y), HorizontalAlignment::Left, day_ink);
    // right side, drawn right to left: battery, room temperature, place + time
    let mut right = BOX_W as i32 - PAD;
    if let Some(pct) = info.battery_pct {
        let label = format!("{pct}%");
        pen.text(&mut t, &SMALL_FONT, &label, at(right, HEADER_Y + 2), HorizontalAlignment::Right, Ink::Black);
        right -= text_width(&SMALL_FONT, &label) + 3;
        battery_icon(&mut t, at(right - 16, HEADER_Y + 3), pct, pen.halo);
        right -= 16 + 8;
    }
    if let Some(c) = info.room_c {
        let label = format!("{}°", c.round() as i32);
        pen.text(&mut t, &SMALL_FONT, &label, at(right, HEADER_Y + 2), HorizontalAlignment::Right, Ink::Black);
        right -= text_width(&SMALL_FONT, &label) + 2;
        house_icon(&mut t, at(right - 9, HEADER_Y + 2), pen.halo);
        right -= 9 + 8;
    }
    if let Some((rise, set)) = info.sun {
        for (time, up) in [(set, false), (rise, true)] {
            let label = format!("{:02}:{:02}", time.0, time.1);
            pen.text(&mut t, &SMALL_FONT, &label, at(right, HEADER_Y + 2), HorizontalAlignment::Right, Ink::Black);
            right -= text_width(&SMALL_FONT, &label) + 2;
            sun_glyph(&mut t, at(right - 11, HEADER_Y + 2), up, pen.halo);
            right -= 11 + 6;
        }
        right -= 2;
    }
    // status holds the configured place name, which may be Cyrillic or Hebrew
    let status_x = right - mixed_width(info.status);
    pen.mixed(&mut t, info.status, at(status_x, HEADER_Y + 1), Ink::Black);
    let _ = Line::new(at(PAD, RULE_Y), at(BOX_W as i32 - PAD, RULE_Y))
        .into_styled(PrimitiveStyle::with_stroke(Ink::Black, 1))
        .draw(&mut t);

    let events = match info.events {
        None => {
            pen.text(&mut t, &TITLE_FONT, "calendar unavailable", at(PAD, ROW_Y + 8), HorizontalAlignment::Left, Ink::Black);
            return;
        }
        Some([]) => {
            pen.text(&mut t, &TITLE_FONT, "No more events today", at(PAD, ROW_Y + 8), HorizontalAlignment::Left, Ink::Black);
            return;
        }
        Some(e) => e,
    };
    // titles start just after the widest possible time label
    let time_w = SMALL_FONT
        .get_rendered_dimensions("00:00-00:00", Point::zero(), VerticalPosition::Top)
        .map_or(80, |d| d.advance.x);
    let title_x = PAD + time_w + 8;
    let shown = if events.len() > EVENT_ROWS { EVENT_ROWS - 1 } else { events.len() };
    for (i, e) in events.iter().take(shown).enumerate() {
        let y = ROW_Y + ROW_H * i as i32;
        let time = event_time_label(e);
        let ink = if e.start.is_some() { Ink::Black } else { Ink::Red };
        pen.text(&mut t, &SMALL_FONT, &time, at(PAD, y + 2), HorizontalAlignment::Left, ink);
        // titles are clipped at the box edge
        let row = Rectangle::new(at(title_x, y), Size::new(BOX_W - title_x as u32 - PAD as u32, ROW_H as u32));
        pen.mixed(&mut t.clipped(&row), &e.title, at(title_x, y + 1), Ink::Black);
    }
    if shown < events.len() {
        let y = ROW_Y + ROW_H * shown as i32;
        let more = format!("+{} more", events.len() - shown);
        pen.text(&mut t, &SMALL_FONT, &more, at(title_x, y + 2), HorizontalAlignment::Left, Ink::Blue);
    }
}

fn text_width(font: &FontRenderer, s: &str) -> i32 {
    font.get_rendered_dimensions(s, Point::zero(), VerticalPosition::Top).map_or(0, |d| d.advance.x)
}

/// 16x8 battery: outline, nub, fill by charge (green, red below 20 %).
fn battery_icon<D: DrawTarget<Color = Ink>>(t: &mut D, p: Point, pct: u8, plate: bool) {
    if plate {
        let _ = Rectangle::new(p - Point::new(1, 1), Size::new(18, 10)).into_styled(PrimitiveStyle::with_fill(Ink::White)).draw(t);
    }
    let body = PrimitiveStyleBuilder::new().stroke_color(Ink::Black).stroke_width(1).fill_color(Ink::White).build();
    let _ = Rectangle::new(p, Size::new(14, 8)).into_styled(body).draw(t);
    let _ = Rectangle::new(p + Point::new(14, 2), Size::new(2, 4)).into_styled(PrimitiveStyle::with_fill(Ink::Black)).draw(t);
    let w = (12 * u32::from(pct.min(100)) + 50) / 100;
    if w > 0 {
        let ink = if pct < 20 { Ink::Red } else { Ink::Green };
        let _ = Rectangle::new(p + Point::new(1, 1), Size::new(w, 6)).into_styled(PrimitiveStyle::with_fill(ink)).draw(t);
    }
}

/// 11x10 sunrise/sunset glyph: half sun on the horizon with an up/down arrow.
fn sun_glyph<D: DrawTarget<Color = Ink>>(t: &mut D, p: Point, up: bool, plate: bool) {
    if plate {
        let _ = Rectangle::new(p - Point::new(1, 1), Size::new(13, 12)).into_styled(PrimitiveStyle::with_fill(Ink::White)).draw(t);
    }
    // half disc above the horizon line
    let disc = PrimitiveStyleBuilder::new().fill_color(Ink::Yellow).stroke_color(Ink::Red).stroke_width(1).build();
    let _ = Circle::with_center(p + Point::new(5, 9), 9)
        .into_styled(disc)
        .draw(&mut t.clipped(&Rectangle::new(p, Size::new(11, 9))));
    let _ = Line::new(p + Point::new(0, 9), p + Point::new(10, 9)).into_styled(PrimitiveStyle::with_stroke(Ink::Black, 1)).draw(t);
    // arrow above the sun
    let tip = if up { (p + Point::new(5, 0), p + Point::new(2, 3), p + Point::new(8, 3)) } else { (p + Point::new(5, 3), p + Point::new(2, 0), p + Point::new(8, 0)) };
    let _ = Triangle::new(tip.0, tip.1, tip.2).into_styled(PrimitiveStyle::with_fill(Ink::Black)).draw(t);
}

/// 9x9 house: roof and walls (room temperature).
fn house_icon<D: DrawTarget<Color = Ink>>(t: &mut D, p: Point, plate: bool) {
    if plate {
        let _ = Rectangle::new(p - Point::new(1, 1), Size::new(11, 11)).into_styled(PrimitiveStyle::with_fill(Ink::White)).draw(t);
    }
    let line = PrimitiveStyle::with_stroke(Ink::Black, 1);
    let _ = Triangle::new(p + Point::new(0, 4), p + Point::new(4, 0), p + Point::new(8, 4)).into_styled(line).draw(t);
    let walls = PrimitiveStyleBuilder::new().stroke_color(Ink::Black).stroke_width(1).fill_color(Ink::White).build();
    let _ = Rectangle::new(p + Point::new(1, 4), Size::new(7, 5)).into_styled(walls).draw(t);
}

/// Draws `icon` centred on `center`, inside an `ICON_SIZE` square.
/// `night` = moon phase: the sun is replaced by the moon.
/// Shapes are designed on a 40 px grid and scaled to `ICON_SIZE`.
pub fn draw_icon<D: DrawTarget<Color = Ink>>(target: &mut D, icon: Icon, center: Point, night: Option<f32>) {
    let half = (ICON_SIZE / 2) as i32;
    let square = Rectangle::new(center - Point::new(half, half), Size::new(ICON_SIZE, ICON_SIZE));
    let mut t = target.clipped(&square);
    let g = Grid { c: center };
    if night.is_some() {
        // night: each hour gets its own black sky tile, with stars when clear
        let _ = square.into_styled(PrimitiveStyle::with_fill(Ink::Black)).draw(&mut t);
        if matches!(icon, Icon::Clear | Icon::PartlyCloudy) {
            stars(&mut t, &g);
        }
    }
    let line_ink = if night.is_some() { Ink::White } else { Ink::Black };
    match icon {
        Icon::Clear => match night {
            Some(phase) => moon(&mut t, &g, (0, 0), 30, phase),
            None => sun(&mut t, &g, (0, 0), 18),
        },
        Icon::PartlyCloudy => {
            match night {
                Some(phase) => moon(&mut t, &g, (8, -8), 20, phase),
                None => sun(&mut t, &g, (8, -8), 13),
            }
            cloud(&mut t, &g, (-2, 5));
        }
        Icon::Cloudy => cloud(&mut t, &g, (0, 2)),
        Icon::Fog => {
            for (y, x0, x1) in [(-10, -14, 12), (-2, -17, 17), (6, -12, 15), (14, -16, 10)] {
                let _ = Line::new(g.p(x0, y), g.p(x1, y))
                    .into_styled(PrimitiveStyle::with_stroke(line_ink, 2))
                    .draw(&mut t);
            }
        }
        Icon::Drizzle => {
            cloud(&mut t, &g, (0, -3));
            for (x, y) in [(-8, 15), (1, 17), (10, 15)] {
                let _ = Circle::with_center(g.p(x, y), 4).into_styled(PrimitiveStyle::with_fill(Ink::Blue)).draw(&mut t);
            }
        }
        Icon::Rain => {
            cloud(&mut t, &g, (0, -3));
            drops(&mut t, &g, (0, 0));
        }
        Icon::Showers => {
            match night {
                Some(phase) => moon(&mut t, &g, (9, -9), 18, phase),
                None => sun(&mut t, &g, (9, -9), 12),
            }
            cloud(&mut t, &g, (-2, -2));
            drops(&mut t, &g, (-2, 1));
        }
        Icon::Thunder => {
            cloud(&mut t, &g, (0, -3));
            let bolt = PrimitiveStyleBuilder::new().fill_color(Ink::Yellow).stroke_color(Ink::Black).stroke_width(1).build();
            let _ = Triangle::new(g.p(4, 8), g.p(-5, 15), g.p(1, 15)).into_styled(bolt).draw(&mut t);
            let _ = Triangle::new(g.p(-1, 13), g.p(5, 13), g.p(-4, 20)).into_styled(bolt).draw(&mut t);
        }
        Icon::Snow => {
            cloud(&mut t, &g, (0, -3));
            for (x, y) in [(-8, 15), (1, 17), (10, 15)] {
                flake(&mut t, g.p(x, y));
            }
        }
    }
}

/// Maps the 40 px design grid onto `ICON_SIZE`.
struct Grid {
    c: Point,
}

impl Grid {
    fn k(&self, v: i32) -> i32 {
        v * ICON_SIZE as i32 / 40
    }
    fn p(&self, x: i32, y: i32) -> Point {
        self.c + Point::new(self.k(x), self.k(y))
    }
    fn d(&self, d: u32) -> u32 {
        (d * ICON_SIZE / 40).max(2)
    }
}

fn sun<D: DrawTarget<Color = Ink>>(t: &mut D, g: &Grid, at: (i32, i32), d: u32) {
    let c = g.p(at.0, at.1);
    let r = g.d(d) as f32 / 2.0;
    let ray = PrimitiveStyle::with_stroke(Ink::Red, 2);
    for k in 0..8 {
        let (s, co) = (k as f32 * std::f32::consts::FRAC_PI_4).sin_cos();
        let from = c + Point::new((co * (r + 2.0)) as i32, (s * (r + 2.0)) as i32);
        let to = c + Point::new((co * (r + 5.0)) as i32, (s * (r + 5.0)) as i32);
        let _ = Line::new(from, to).into_styled(ray).draw(t);
    }
    let disc = PrimitiveStyleBuilder::new().fill_color(Ink::Yellow).stroke_color(Ink::Red).stroke_width(1).build();
    let _ = Circle::with_center(c, g.d(d)).into_styled(disc).draw(t);
}

/// The moon in its phase: lit part yellow, dark part black, thin outline.
/// Waxing (phase < 0.5) is lit on the right, waning on the left.
fn moon<D: DrawTarget<Color = Ink>>(t: &mut D, g: &Grid, at: (i32, i32), d: u32, phase: f32) {
    let c = g.p(at.0, at.1);
    let r = g.d(d) as f32 / 2.0;
    let k = (2.0 * std::f32::consts::PI * phase).cos();
    let ri = r.ceil() as i32;
    let pixels = (-ri..=ri).flat_map(|dy| (-ri..=ri).map(move |dx| (dx, dy))).filter_map(|(dx, dy)| {
        let (x, y) = ((dx as f32 + 0.5) / r, (dy as f32 + 0.5) / r);
        if x * x + y * y > 1.0 {
            return None;
        }
        let edge = (1.0 - y * y).max(0.0).sqrt();
        let lit = if phase < 0.5 { x >= k * edge } else { x <= -k * edge };
        Some(Pixel(c + Point::new(dx, dy), if lit { Ink::Yellow } else { Ink::Black }))
    });
    let _ = t.draw_iter(pixels);
    // the dark side vanishes in the black sky; a blue ring keeps the disc visible
    let _ = Circle::with_center(c, g.d(d) + 1).into_styled(PrimitiveStyle::with_stroke(Ink::Blue, 1)).draw(t);
}

/// A few stars in the corners of a night tile (clear of the moon).
fn stars<D: DrawTarget<Color = Ink>>(t: &mut D, g: &Grid) {
    let white = PrimitiveStyle::with_fill(Ink::White);
    // small crosses
    for (x, y) in [(-16, -15), (15, 13)] {
        let c = g.p(x, y);
        let _ = Rectangle::new(c - Point::new(1, 0), Size::new(3, 1)).into_styled(white).draw(t);
        let _ = Rectangle::new(c - Point::new(0, 1), Size::new(1, 3)).into_styled(white).draw(t);
    }
    // single dots, one yellow
    for (x, y, ink) in [(14, -16, Ink::White), (-15, 15, Ink::White), (-17, 2, Ink::Yellow), (17, -3, Ink::White)] {
        let _ = t.draw_iter([Pixel(g.p(x, y), ink)]);
    }
}

/// A cloud as the union of three puffs and a base, with a black outline:
/// draw every part black and slightly bigger, then every part white on top.
fn cloud<D: DrawTarget<Color = Ink>>(t: &mut D, g: &Grid, at: (i32, i32)) {
    let (cx, cy) = at;
    let puffs = [((cx - 8, cy + 2), 16u32), ((cx + 1, cy - 4), 22), ((cx + 10, cy + 3), 14)];
    let base = Rectangle::with_corners(g.p(cx - 16, cy + 2), g.p(cx + 17, cy + 10));
    for (ink, grow) in [(Ink::Black, 2i32), (Ink::White, 0)] {
        let fill = PrimitiveStyle::with_fill(ink);
        for ((x, y), d) in puffs {
            let _ = Circle::with_center(g.p(x, y), g.d(d) + 2 * grow as u32).into_styled(fill).draw(t);
        }
        let _ = base.offset(grow).into_styled(fill).draw(t);
    }
}

fn drops<D: DrawTarget<Color = Ink>>(t: &mut D, g: &Grid, at: (i32, i32)) {
    for x in [-8, 1, 10] {
        let _ = Line::new(g.p(at.0 + x, at.1 + 12), g.p(at.0 + x - 3, at.1 + 19))
            .into_styled(PrimitiveStyle::with_stroke(Ink::Blue, 2))
            .draw(t);
    }
}

fn flake<D: DrawTarget<Color = Ink>>(t: &mut D, c: Point) {
    let s = PrimitiveStyle::with_stroke(Ink::Blue, 1);
    for (dx, dy) in [(2, 0), (0, 2), (2, 2), (2, -2)] {
        let _ = Line::new(c - Point::new(dx, dy), c + Point::new(dx, dy)).into_styled(s).draw(t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(temp: f32) -> Vec<HourSlot> {
        (0..6)
            .map(|i| HourSlot { hour: 6 + i, temp_c: temp + f32::from(i), precip_pct: i * 10, uv: 0.0, is_day: true, moon: 0.0, icon: Icon::Rain })
            .collect()
    }

    fn events(n: usize) -> Vec<DayEvent> {
        let mut v = vec![DayEvent { start: None, end: None, title: "ДР у друга".into() }];
        let titles = ["Иврит / Язык (שפה) - учитель", "Математика (חשבון)", "Акробатика", "Музыка", "תור טלפוני", "Наука"];
        for i in 1..n {
            v.push(DayEvent { start: Some((7 + i as u8, 30)), end: Some((8 + i as u8, 15)), title: titles[(i - 1) % titles.len()].into() });
        }
        v.truncate(n);
        v
    }

    fn weather(forecast: Option<&[HourSlot]>) -> FrameBuffer {
        let mut fb = FrameBuffer::new(Ink::Green);
        draw_weather_box(&mut fb, forecast, Backdrop::Solid);
        fb
    }

    fn agenda(ev: Option<&[DayEvent]>) -> FrameBuffer {
        let mut fb = FrameBuffer::new(Ink::Green);
        draw_events_box(&mut fb, &EventsInfo { date: "Wed 25 Nov", status: "Tel Aviv 06:00", room_c: None, battery_pct: None, events: ev, tomorrow: false, sun: None }, Backdrop::Solid);
        fb
    }

    fn inside(r: &Rectangle, x: u32, y: u32) -> bool {
        r.contains(Point::new(x as i32, y as i32))
    }

    fn untouched_outside(fb: &FrameBuffer, r: &Rectangle) {
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                if !inside(r, x, y) {
                    assert_eq!(fb.get(x, y), Ink::Green, "pixel ({x},{y}) outside the box changed");
                }
            }
        }
    }

    fn ink_in_body(fb: &FrameBuffer, r: &Rectangle, from_y: u32) -> bool {
        let (x0, y0) = (r.top_left.x as u32, r.top_left.y as u32);
        (from_y..r.size.height - 3).any(|dy| (3..r.size.width - 3).any(|dx| fb.get(x0 + dx, y0 + dy) == Ink::Black))
    }

    #[test]
    fn rain_shows_only_above_zero_otherwise_high_uv() {
        let s = |precip_pct, uv| HourSlot { hour: 12, temp_c: 30.0, precip_pct, uv, is_day: true, moon: 0.0, icon: Icon::Clear };
        assert_eq!(rain_or_uv(&s(0, 2.0)), None, "no rain, low UV: empty");
        assert_eq!(rain_or_uv(&s(30, 2.0)), Some(("30%".into(), Ink::Blue)));
        assert_eq!(rain_or_uv(&s(0, 7.4)), Some(("UV 7".into(), Ink::Red)));
        assert_eq!(rain_or_uv(&s(0, 6.0)), Some(("UV 6".into(), Ink::Red)));
        assert_eq!(rain_or_uv(&s(0, 5.9)), None);
        assert_eq!(rain_or_uv(&s(20, 9.0)), Some(("20%".into(), Ink::Blue)), "rain wins");
    }

    #[test]
    fn weather_hours_are_full_times() {
        assert_eq!(hour_label(6), "06:00");
        assert_eq!(hour_label(14), "14:00");
    }

    #[test]
    fn event_times_show_start_and_end() {
        let e = |start, end| DayEvent { start, end, title: String::new() };
        assert_eq!(event_time_label(&e(Some((13, 0)), Some((14, 0)))), "13:00-14:00");
        assert_eq!(event_time_label(&e(Some((8, 5)), Some((8, 50)))), "08:05-08:50");
        assert_eq!(event_time_label(&e(Some((22, 0)), None)), "22:00-...");
        assert_eq!(event_time_label(&e(None, None)), "all day");
    }

    #[test]
    fn boxes_are_mirrored_and_cover_at_most_20_percent() {
        let (e, w) = (events_rect(), weather_rect());
        assert_eq!(e.size, w.size);
        assert_eq!(e.top_left.y, w.top_left.y, "same height on screen");
        assert!(e.intersection(&w).is_zero_sized(), "boxes overlap");
        let left_margin = e.top_left.x;
        let right_margin = WIDTH as i32 - 1 - w.bottom_right().unwrap().x;
        assert_eq!(left_margin, right_margin, "not symmetric");
        let bottom = HEIGHT as i32 - 1 - e.bottom_right().unwrap().y;
        assert!(bottom >= 0 && bottom <= 16, "not at the bottom: {bottom}");
        let covered = 2 * e.size.width * e.size.height;
        assert!(covered * 5 <= WIDTH * HEIGHT, "boxes cover {covered} px, more than 20 %");
    }

    #[test]
    fn weather_box_stays_inside_and_covers_the_photo() {
        let s = slots(14.0);
        let fb = weather(Some(&s));
        let r = weather_rect();
        untouched_outside(&fb, &r);
        let (x, y) = (r.top_left.x as u32, r.top_left.y as u32);
        assert_eq!(fb.get(x, y), Ink::Black, "border");
        assert_eq!(fb.get(x + 3, y + 3), Ink::White, "background");
        let no_photo = (0..r.size.height).all(|dy| (0..r.size.width).all(|dx| fb.get(x + dx, y + dy) != Ink::Green));
        assert!(no_photo, "box background must cover the photo");
    }

    #[test]
    fn forecast_values_are_rendered() {
        let (a, b) = (slots(14.0), slots(-3.0));
        assert_ne!(weather(Some(&a)).as_bytes(), weather(Some(&b)).as_bytes());
    }

    #[test]
    fn missing_weather_still_draws_a_message() {
        assert!(ink_in_body(&weather(None), &weather_rect(), 30));
    }

    #[test]
    fn events_box_stays_inside_and_covers_the_photo() {
        let ev = events(3);
        let fb = agenda(Some(&ev));
        let r = events_rect();
        untouched_outside(&fb, &r);
        let (x, y) = (r.top_left.x as u32, r.top_left.y as u32);
        assert_eq!(fb.get(x, y), Ink::Black, "border");
        let no_photo = (0..r.size.height).all(|dy| (0..r.size.width).all(|dx| fb.get(x + dx, y + dy) != Ink::Green));
        assert!(no_photo, "box background must cover the photo");
    }

    #[test]
    fn events_are_listed() {
        let ev = events(3);
        assert_ne!(agenda(Some(&ev)).as_bytes(), agenda(Some(&[])).as_bytes());
        assert!(ink_in_body(&agenda(Some(&ev)), &events_rect(), 26));
    }

    #[test]
    fn overflow_shows_how_many_more() {
        // 6 vs 7 events: same first lines, different "+N more"
        let (six, seven) = (events(6), events(7));
        assert_ne!(agenda(Some(&six)).as_bytes(), agenda(Some(&seven)).as_bytes());
        // exactly EVENT_ROWS events fit without a "more" line, so one more changes the last line
        let (fit, over) = (events(EVENT_ROWS), events(EVENT_ROWS + 1));
        assert_ne!(agenda(Some(&fit)).as_bytes(), agenda(Some(&over)).as_bytes());
    }

    #[test]
    fn empty_day_and_missing_calendar_show_a_message() {
        assert!(ink_in_body(&agenda(Some(&[])), &events_rect(), 26));
        assert!(ink_in_body(&agenda(None), &events_rect(), 26));
        assert_ne!(agenda(Some(&[])).as_bytes(), agenda(None).as_bytes());
    }

    #[test]
    fn clear_boxes_let_the_photo_show_through() {
        let s = slots(14.0);
        let ev = events(3);
        let mut fb = FrameBuffer::new(Ink::Green);
        draw_weather_box(&mut fb, Some(&s), Backdrop::Clear);
        draw_events_box(&mut fb, &EventsInfo { date: "Wed 25 Nov", status: "x", room_c: None, battery_pct: None, events: Some(&ev), tomorrow: false, sun: None }, Backdrop::Clear);
        for r in [weather_rect(), events_rect()] {
            untouched_outside_both(&fb);
            let (x0, y0) = (r.top_left.x as u32, r.top_left.y as u32);
            let count = |ink| {
                (0..r.size.height).flat_map(|dy| (0..r.size.width).map(move |dx| (dx, dy))).filter(|&(dx, dy)| fb.get(x0 + dx, y0 + dy) == ink).count()
            };
            let area = (r.size.width * r.size.height) as usize;
            assert!(count(Ink::Green) > area / 2, "photo should mostly show through");
            assert!(count(Ink::Black) > 200, "text and border");
            assert!(count(Ink::White) > 200, "white outline around text");
        }
    }

    fn untouched_outside_both(fb: &FrameBuffer) {
        let (e, w) = (events_rect(), weather_rect());
        for y in (0..HEIGHT).step_by(7) {
            for x in (0..WIDTH).step_by(7) {
                if !inside(&e, x, y) && !inside(&w, x, y) {
                    assert_eq!(fb.get(x, y), Ink::Green);
                }
            }
        }
    }

    fn header(room_c: Option<f32>, battery_pct: Option<u8>) -> FrameBuffer {
        let mut fb = FrameBuffer::new(Ink::Green);
        let info = EventsInfo { date: "Wed 25 Nov", status: "Tel Aviv 06:00", room_c, battery_pct, events: Some(&[]), tomorrow: false, sun: None };
        draw_events_box(&mut fb, &info, Backdrop::Solid);
        fb
    }

    fn header_count(fb: &FrameBuffer, ink: Ink) -> usize {
        let r = events_rect();
        let (x0, y0) = (r.top_left.x as u32, r.top_left.y as u32);
        (2..RULE_Y as u32).flat_map(|dy| (2..r.size.width - 2).map(move |dx| (dx, dy))).filter(|&(dx, dy)| fb.get(x0 + dx, y0 + dy) == ink).count()
    }

    #[test]
    fn header_shows_room_temperature_and_battery() {
        let plain = header(None, None);
        assert_ne!(header(Some(23.4), None).as_bytes(), plain.as_bytes(), "room temperature");
        assert_ne!(header(None, Some(87)).as_bytes(), plain.as_bytes(), "battery");
        assert_ne!(header(Some(23.4), Some(87)).as_bytes(), header(Some(23.4), Some(30)).as_bytes(), "battery level");
        assert_ne!(header(Some(23.4), None).as_bytes(), header(Some(18.0), None).as_bytes(), "temperature value");
    }

    #[test]
    fn tomorrows_events_say_tomorrow_in_blue() {
        let ev = events(2);
        let draw = |tomorrow| {
            let mut fb = FrameBuffer::new(Ink::Green);
            let info = EventsInfo { date: "Wed 25 Nov", status: "x", room_c: None, battery_pct: None, events: Some(&ev), tomorrow, sun: None };
            draw_events_box(&mut fb, &info, Backdrop::Solid);
            fb
        };
        let (today, tomorrow) = (draw(false), draw(true));
        assert_ne!(today.as_bytes(), tomorrow.as_bytes());
        assert!(header_count(&tomorrow, Ink::Blue) > 20, "'Tomorrow' is blue");
        assert_eq!(header_count(&today, Ink::Blue), 0);
    }

    #[test]
    fn low_battery_is_red() {
        assert!(header_count(&header(None, Some(10)), Ink::Red) > 3);
        assert_eq!(header_count(&header(None, Some(90)), Ink::Red), 0);
    }

    fn icon_fb(icon: Icon, night: Option<f32>) -> FrameBuffer {
        let mut fb = FrameBuffer::new(Ink::Green);
        draw_icon(&mut fb, icon, Point::new(100, 100), night);
        fb
    }

    fn count(fb: &FrameBuffer, ink: Ink) -> usize {
        (70..130).flat_map(|y| (70..130).map(move |x| (x, y))).filter(|&(x, y)| fb.get(x, y) == ink).count()
    }

    fn icon_inks(icon: Icon, night: Option<f32>) -> (usize, usize, usize) {
        let fb = icon_fb(icon, night);
        (count(&fb, Ink::Yellow), count(&fb, Ink::Red), count(&fb, Ink::Black))
    }

    #[test]
    fn night_hours_have_a_black_sky_tile() {
        let square = (ICON_SIZE * ICON_SIZE) as usize;
        for icon in [Icon::Clear, Icon::PartlyCloudy, Icon::Cloudy, Icon::Rain, Icon::Fog, Icon::Snow, Icon::Thunder] {
            let fb = icon_fb(icon, Some(0.3));
            assert_eq!(count(&fb, Ink::Green), 60 * 60 - square, "{icon:?}: the tile covers the whole icon square");
            assert!(count(&fb, Ink::Black) > square / 4, "{icon:?}: black sky");
        }
        assert_eq!(count(&icon_fb(Icon::Clear, None), Ink::Black), 0, "no tile by day");
    }

    #[test]
    fn clear_night_sky_has_stars() {
        assert!(count(&icon_fb(Icon::Clear, Some(0.5)), Ink::White) >= 4, "stars");
        assert!(count(&icon_fb(Icon::Fog, Some(0.5)), Ink::White) > 20, "fog lines turn white at night");
    }

    #[test]
    fn clear_night_shows_the_moon_not_the_sun() {
        let (_, red_day, _) = icon_inks(Icon::Clear, None);
        assert!(red_day > 10, "the sun has red rays");
        let (yellow, red, _) = icon_inks(Icon::Clear, Some(0.5));
        assert_eq!(red, 0, "no sun rays at night");
        assert!(yellow > 100, "full moon");
        let (_, red, _) = icon_inks(Icon::PartlyCloudy, Some(0.5));
        assert_eq!(red, 0, "partly cloudy night: moon behind the cloud");
    }

    #[test]
    fn moon_shows_its_phase() {
        let lit = |p| icon_inks(Icon::Clear, Some(p)).0;
        assert!(lit(0.5) > lit(0.25) + 50, "full > first quarter");
        assert!(lit(0.25) > lit(0.05) + 50, "first quarter > thin crescent");
        let fb = icon_fb(Icon::Clear, Some(0.0));
        assert!(count(&fb, Ink::Yellow) < 10 && count(&fb, Ink::Blue) > 10, "new moon: dark, its disc outlined");
        assert!((lit(0.25) as i32 - lit(0.75) as i32).abs() < 20, "first and last quarter equally lit");
    }

    #[test]
    fn header_shows_sunrise_and_sunset() {
        let draw = |sun| {
            let mut fb = FrameBuffer::new(Ink::Green);
            let info = EventsInfo { date: "Wed 25 Nov", status: "19:10", room_c: None, battery_pct: None, events: Some(&[]), tomorrow: false, sun };
            draw_events_box(&mut fb, &info, Backdrop::Solid);
            fb
        };
        let with = draw(Some(((6, 21), (16, 38))));
        assert_ne!(with.as_bytes(), draw(None).as_bytes());
        assert!(header_count(&with, Ink::Yellow) > 5, "small sun glyphs");
        assert_ne!(with.as_bytes(), draw(Some(((7, 1), (17, 2)))).as_bytes(), "times are drawn");
    }

    #[test]
    fn every_icon_draws_inside_its_square() {
        let c = Point::new(100, 100);
        let half = (ICON_SIZE / 2) as i32;
        let square = Rectangle::new(c - Point::new(half, half), Size::new(ICON_SIZE, ICON_SIZE));
        for icon in [
            Icon::Clear,
            Icon::PartlyCloudy,
            Icon::Cloudy,
            Icon::Fog,
            Icon::Drizzle,
            Icon::Rain,
            Icon::Showers,
            Icon::Thunder,
            Icon::Snow,
        ] {
            let mut fb = FrameBuffer::new(Ink::White);
            draw_icon(&mut fb, icon, c, None);
            let mut drawn = 0;
            for y in 0..200 {
                for x in 0..200 {
                    if fb.get(x, y) != Ink::White {
                        assert!(inside(&square, x, y), "{icon:?} drew outside its square at ({x},{y})");
                        drawn += 1;
                    }
                }
            }
            assert!(drawn > 20, "{icon:?} drew almost nothing");
        }
    }
}
