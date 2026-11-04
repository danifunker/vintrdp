//! Draws vintrdp's built-in pictures: a CRT monitor whose screen shows two
//! offset chevrons (a ">" low on the left and a "<" high on the right, with
//! space between them), in eight casing colours, plus the app icon.
//!
//!   cargo run -p vintrdp --example make_icons
//!
//! writes PNGs into crates/vintrdp/assets/. The app embeds those files, so
//! rerun this only when the artwork changes.

use std::path::Path;

use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, Path as SkPath, PathBuilder, Pixmap, Point,
    RadialGradient, SpreadMode, Stroke, Transform,
};

/// Name, casing colour, phosphor (chevron glow) colour.
pub const VARIANTS: [(&str, [u8; 3], [u8; 3]); 8] = [
    ("beige", [222, 210, 182], [96, 255, 160]),
    ("graphite", [92, 98, 112], [130, 205, 255]),
    ("red", [204, 58, 64], [255, 160, 150]),
    ("amber", [232, 164, 44], [255, 210, 110]),
    ("green", [70, 164, 96], [150, 255, 180]),
    ("teal", [28, 150, 162], [120, 245, 240]),
    ("blue", [52, 104, 206], [140, 190, 255]),
    ("violet", [134, 84, 196], [215, 170, 255]),
];

/// Which variant the app icon uses.
const APP_ICON_VARIANT: usize = 6;

fn rgb(c: [u8; 3], a: u8) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], a)
}

/// Scale a colour towards black (k < 1) or white (k > 1).
fn shade(c: [u8; 3], k: f32) -> [u8; 3] {
    c.map(|v| {
        let v = v as f32;
        (if k <= 1.0 { v * k } else { v + (255.0 - v) * (k - 1.0) }).round().clamp(0.0, 255.0) as u8
    })
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> SkPath {
    // Cubic approximation of quarter circles.
    let k = 0.552_284_8 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish().unwrap()
}

fn solid(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c);
    p.anti_alias = true;
    p
}

fn vertical(top: Color, bottom: Color, y0: f32, y1: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.anti_alias = true;
    p.shader = LinearGradient::new(
        Point::from_xy(0.0, y0),
        Point::from_xy(0.0, y1),
        vec![GradientStop::new(0.0, top), GradientStop::new(1.0, bottom)],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap();
    p
}

/// A chevron through three points, as a thick rounded stroke.
fn chevron(points: [(f32, f32); 3]) -> SkPath {
    let mut pb = PathBuilder::new();
    pb.move_to(points[0].0, points[0].1);
    pb.line_to(points[1].0, points[1].1);
    pb.line_to(points[2].0, points[2].1);
    pb.finish().unwrap()
}

/// Draw the monitor on a 1024x1024 design grid, scaled to `size`.
fn draw(size: u32, casing: [u8; 3], glow: [u8; 3]) -> Pixmap {
    let mut pm = Pixmap::new(size, size).unwrap();
    let t = Transform::from_scale(size as f32 / 1024.0, size as f32 / 1024.0);
    let fill = |pm: &mut Pixmap, path: &SkPath, paint: &Paint| pm.fill_path(path, paint, FillRule::Winding, t, None);

    // Soft shadow under the stand
    let mut shadow = Paint::default();
    shadow.anti_alias = true;
    shadow.shader = RadialGradient::new(
        Point::from_xy(512.0, 930.0),
        0.0,
        Point::from_xy(512.0, 930.0),
        330.0,
        vec![GradientStop::new(0.0, Color::from_rgba8(0, 0, 0, 90)), GradientStop::new(1.0, Color::from_rgba8(0, 0, 0, 0))],
        SpreadMode::Pad,
        Transform::from_scale(1.0, 0.16).pre_translate(0.0, 930.0 / 0.16 - 930.0),
    )
    .unwrap();
    fill(&mut pm, &PathBuilder::from_rect(tiny_skia::Rect::from_xywh(150.0, 880.0, 724.0, 100.0).unwrap()), &shadow);

    // Stand: neck and foot
    let dark = shade(casing, 0.72);
    let mut neck = PathBuilder::new();
    neck.move_to(430.0, 790.0);
    neck.line_to(594.0, 790.0);
    neck.line_to(620.0, 872.0);
    neck.line_to(404.0, 872.0);
    neck.close();
    fill(&mut pm, &neck.finish().unwrap(), &vertical(rgb(shade(casing, 0.62), 255), rgb(dark, 255), 790.0, 872.0));
    fill(&mut pm, &rounded_rect(300.0, 858.0, 424.0, 64.0, 26.0), &vertical(rgb(shade(casing, 1.12), 255), rgb(dark, 255), 858.0, 922.0));

    // Casing, with a light top edge
    let body = rounded_rect(104.0, 108.0, 816.0, 704.0, 96.0);
    fill(&mut pm, &body, &vertical(rgb(shade(casing, 1.18), 255), rgb(shade(casing, 0.82), 255), 108.0, 812.0));
    let mut edge = Stroke::default();
    edge.width = 6.0;
    pm.stroke_path(&rounded_rect(110.0, 114.0, 804.0, 692.0, 90.0), &solid(rgb(shade(casing, 1.4), 120)), &edge, t, None);

    // Bezel recess around the tube
    fill(&mut pm, &rounded_rect(170.0, 170.0, 684.0, 528.0, 70.0), &vertical(rgb(shade(casing, 0.55), 255), rgb(shade(casing, 0.9), 255), 170.0, 698.0));

    // The tube: rounder corners than the bezel, dark glass lit from the middle
    let glass = rounded_rect(196.0, 194.0, 632.0, 480.0, 88.0);
    let mut tube = Paint::default();
    tube.anti_alias = true;
    tube.shader = RadialGradient::new(
        Point::from_xy(500.0, 410.0),
        0.0,
        Point::from_xy(512.0, 434.0),
        420.0,
        vec![
            GradientStop::new(0.0, rgb(shade(glow, 0.42), 255)),
            GradientStop::new(0.6, rgb(shade(glow, 0.20), 255)),
            GradientStop::new(1.0, rgb(shade(glow, 0.07), 255)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap();
    fill(&mut pm, &glass, &tube);

    // The mark: ">" low-left and "<" high-right, a clear gap between them
    let (cx, cy) = (512.0, 434.0);
    let (arm, depth, dx, dy) = (78.0, 74.0, 84.0, 46.0);
    let right = {
        let (x, y) = (cx - dx, cy + dy);
        chevron([(x - depth / 2.0, y - arm), (x + depth / 2.0, y), (x - depth / 2.0, y + arm)])
    };
    let left = {
        let (x, y) = (cx + dx, cy - dy);
        chevron([(x + depth / 2.0, y - arm), (x - depth / 2.0, y), (x + depth / 2.0, y + arm)])
    };
    let mut stroke = Stroke::default();
    stroke.line_cap = LineCap::Round;
    stroke.line_join = LineJoin::Round;
    // Phosphor glow: many faint, widening strokes add up to a soft falloff.
    let mut layers: Vec<(f32, Color)> = (0..12).rev().map(|i| (50.0 + i as f32 * 9.0, rgb(glow, 13))).collect();
    // The lit stroke, then a hotter core.
    layers.push((48.0, rgb(shade(glow, 1.45), 255)));
    layers.push((18.0, Color::from_rgba8(255, 255, 255, 150)));
    for (width, colour) in layers {
        stroke.width = width;
        for mark in [&right, &left] {
            pm.stroke_path(mark, &solid(colour), &stroke, t, None);
        }
    }

    // Glare across the top-left of the glass
    let mut glare = PathBuilder::new();
    glare.move_to(250.0, 236.0);
    glare.quad_to(420.0, 214.0, 600.0, 220.0);
    glare.quad_to(420.0, 268.0, 236.0, 380.0);
    glare.quad_to(226.0, 290.0, 250.0, 236.0);
    glare.close();
    fill(&mut pm, &glare.finish().unwrap(), &solid(Color::from_rgba8(255, 255, 255, 34)));

    // Front panel: vent slot and power light
    fill(&mut pm, &rounded_rect(214.0, 732.0, 190.0, 18.0, 9.0), &solid(rgb(shade(casing, 0.6), 255)));
    fill(&mut pm, &rounded_rect(214.0, 762.0, 130.0, 14.0, 7.0), &solid(rgb(shade(casing, 0.66), 255)));
    let led = PathBuilder::from_circle(792.0, 750.0, 15.0).unwrap();
    fill(&mut pm, &PathBuilder::from_circle(792.0, 750.0, 30.0).unwrap(), &solid(Color::from_rgba8(110, 255, 140, 50)));
    fill(&mut pm, &led, &solid(Color::from_rgba8(120, 255, 150, 255)));
    pm
}

fn main() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    std::fs::create_dir_all(out.join("monitors")).unwrap();
    for (i, (name, casing, glow)) in VARIANTS.iter().enumerate() {
        let path = out.join("monitors").join(format!("{i}-{name}.png"));
        draw(256, *casing, *glow).save_png(&path).unwrap();
        println!("{}", path.display());
    }
    let (_, casing, glow) = VARIANTS[APP_ICON_VARIANT];
    let path = out.join("icon-1024.png");
    draw(1024, casing, glow).save_png(&path).unwrap();
    println!("{}", path.display());
}
