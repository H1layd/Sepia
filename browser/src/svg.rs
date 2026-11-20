//! A small SVG renderer: the subset icons and logos are made of.
//!
//! Shapes: path (every command, arcs included), rect (rounded), circle,
//! ellipse, line, polyline, polygon, grouped with `g` / nested `svg` and
//! `transform`. Paint: fill and stroke (colour, `none`, `currentColor`,
//! a gradient reference drawn as its first stop), `fill-rule`, opacity,
//! `stroke-width`, presentation attributes and `style="…"`. Not drawn:
//! text, images, filters, masks, clip paths, markers, patterns.
//!
//! Rasterising: contours are flattened to polygons and scan-converted with
//! four sub-rows per pixel and exact horizontal coverage, so edges are
//! anti-aliased. Strokes become one polygon per segment plus round joins,
//! filled together with the non-zero rule. Nothing here touches the network:
//! `<use href>` and external references are ignored.

use crate::css::{parse_color, Color};
use crate::dom::{self, Node};
use crate::images::Bitmap;

/// A 2-D affine transform [a b c d e f]: x' = a x + c y + e, y' = b x + d y + f.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Affine([f32; 6]);

impl Affine {
    const ID: Affine = Affine([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    fn then(self, next: Affine) -> Affine {
        // apply `self` first, then `next`
        let [a, b, c, d, e, f] = self.0;
        let [na, nb, nc, nd, ne, nf] = next.0;
        Affine([
            na * a + nc * b,
            nb * a + nd * b,
            na * c + nc * d,
            nb * c + nd * d,
            na * e + nc * f + ne,
            nb * e + nd * f + nf,
        ])
    }

    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }

    fn scale_factor(&self) -> f32 {
        let [a, b, c, d, _, _] = self.0;
        ((a * d - b * c).abs()).sqrt().max(1e-3)
    }
}

fn parse_transform(text: &str) -> Affine {
    let mut result = Affine::ID;
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().trim_start_matches(',').trim().to_ascii_lowercase();
        let Some(close) = rest[open..].find(')') else { break };
        let args = numbers(&rest[open + 1..open + close]);
        let t = match name.as_str() {
            "translate" => Affine([1.0, 0.0, 0.0, 1.0, args.first().copied().unwrap_or(0.0), args.get(1).copied().unwrap_or(0.0)]),
            "scale" => {
                let sx = args.first().copied().unwrap_or(1.0);
                Affine([sx, 0.0, 0.0, args.get(1).copied().unwrap_or(sx), 0.0, 0.0])
            }
            "rotate" => {
                let (sin, cos) = args.first().copied().unwrap_or(0.0).to_radians().sin_cos();
                let rot = Affine([cos, sin, -sin, cos, 0.0, 0.0]);
                if args.len() >= 3 {
                    let (cx, cy) = (args[1], args[2]);
                    Affine([1.0, 0.0, 0.0, 1.0, -cx, -cy]).then(rot).then(Affine([1.0, 0.0, 0.0, 1.0, cx, cy]))
                } else {
                    rot
                }
            }
            "skewx" => Affine([1.0, 0.0, args.first().copied().unwrap_or(0.0).to_radians().tan(), 1.0, 0.0, 0.0]),
            "skewy" => Affine([1.0, args.first().copied().unwrap_or(0.0).to_radians().tan(), 0.0, 1.0, 0.0, 0.0]),
            "matrix" if args.len() == 6 => Affine([args[0], args[1], args[2], args[3], args[4], args[5]]),
            _ => Affine::ID,
        };
        // transforms in a list apply right to left to the content
        result = t.then(result);
        rest = &rest[open + close + 1..];
    }
    result
}

/// Every number in an SVG number list ("1.5.5-2e3" is 1.5, .5, -2000).
fn numbers(text: &str) -> Vec<f32> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() || c == b'.' || c == b'-' || c == b'+' {
            let start = i;
            i += 1;
            let mut seen_dot = c == b'.';
            while i < bytes.len() {
                let d = bytes[i];
                if d.is_ascii_digit() {
                    i += 1;
                } else if d == b'.' && !seen_dot {
                    seen_dot = true;
                    i += 1;
                } else if (d == b'e' || d == b'E') && i + 1 < bytes.len() && (bytes[i + 1].is_ascii_digit() || bytes[i + 1] == b'-' || bytes[i + 1] == b'+') {
                    i += 2;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    break;
                } else {
                    break;
                }
            }
            if let Ok(n) = text[start..i].parse::<f32>() {
                out.push(n);
            }
        } else {
            i += 1;
        }
    }
    out
}

type Contour = Vec<(f32, f32)>;

/// Path data → contours in user space; `closed[i]` tells whether contour i was closed.
fn parse_path(d: &str) -> (Vec<Contour>, Vec<bool>) {
    let mut contours: Vec<Contour> = Vec::new();
    let mut closed: Vec<bool> = Vec::new();
    let mut current: Contour = Vec::new();
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let (mut start_x, mut start_y) = (0.0f32, 0.0f32);
    let mut last_ctrl: Option<(f32, f32)> = None;
    let mut last_cmd = b' ';
    let bytes = d.as_bytes();
    let mut i = 0;
    let finish = |current: &mut Contour, contours: &mut Vec<Contour>, closed: &mut Vec<bool>, was_closed: bool| {
        if current.len() > 1 {
            contours.push(std::mem::take(current));
            closed.push(was_closed);
        } else {
            current.clear();
        }
    };
    while i < bytes.len() {
        let c = bytes[i];
        if !c.is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let cmd = c;
        i += 1;
        let start = i;
        while i < bytes.len() && !(bytes[i].is_ascii_alphabetic() && bytes[i] != b'e' && bytes[i] != b'E') {
            i += 1;
        }
        let args = numbers(&d[start..i]);
        let rel = cmd.is_ascii_lowercase();
        let up = cmd.to_ascii_uppercase();
        let arity = match up {
            b'M' | b'L' | b'T' => 2,
            b'H' | b'V' => 1,
            b'C' => 6,
            b'S' | b'Q' => 4,
            b'A' => 7,
            _ => 0,
        };
        if up == b'Z' {
            if !current.is_empty() {
                current.push((start_x, start_y));
            }
            finish(&mut current, &mut contours, &mut closed, true);
            x = start_x;
            y = start_y;
            last_cmd = b'Z';
            continue;
        }
        if arity == 0 || args.len() < arity {
            continue;
        }
        for (n, a) in args.chunks_exact(arity).enumerate() {
            let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
            // a moveto's extra pairs are linetos
            let op = if up == b'M' && n > 0 { b'L' } else { up };
            match op {
                b'M' => {
                    finish(&mut current, &mut contours, &mut closed, false);
                    x = ox + a[0];
                    y = oy + a[1];
                    start_x = x;
                    start_y = y;
                    current.push((x, y));
                    last_ctrl = None;
                }
                b'L' => {
                    x = ox + a[0];
                    y = oy + a[1];
                    current.push((x, y));
                    last_ctrl = None;
                }
                b'H' => {
                    x = if rel { x + a[0] } else { a[0] };
                    current.push((x, y));
                    last_ctrl = None;
                }
                b'V' => {
                    y = if rel { y + a[0] } else { a[0] };
                    current.push((x, y));
                    last_ctrl = None;
                }
                b'C' | b'S' => {
                    let (c1, c2, end) = if op == b'C' {
                        ((ox + a[0], oy + a[1]), (ox + a[2], oy + a[3]), (ox + a[4], oy + a[5]))
                    } else {
                        let reflect = match (last_ctrl, matches!(last_cmd.to_ascii_uppercase(), b'C' | b'S')) {
                            (Some((cx, cy)), true) => (2.0 * x - cx, 2.0 * y - cy),
                            _ => (x, y),
                        };
                        (reflect, (ox + a[0], oy + a[1]), (ox + a[2], oy + a[3]))
                    };
                    cubic(&mut current, (x, y), c1, c2, end);
                    last_ctrl = Some(c2);
                    x = end.0;
                    y = end.1;
                }
                b'Q' | b'T' => {
                    let (ctrl, end) = if op == b'Q' {
                        ((ox + a[0], oy + a[1]), (ox + a[2], oy + a[3]))
                    } else {
                        let reflect = match (last_ctrl, matches!(last_cmd.to_ascii_uppercase(), b'Q' | b'T')) {
                            (Some((cx, cy)), true) => (2.0 * x - cx, 2.0 * y - cy),
                            _ => (x, y),
                        };
                        (reflect, (ox + a[0], oy + a[1]))
                    };
                    // a quadratic is a cubic with these controls
                    let c1 = (x + 2.0 / 3.0 * (ctrl.0 - x), y + 2.0 / 3.0 * (ctrl.1 - y));
                    let c2 = (end.0 + 2.0 / 3.0 * (ctrl.0 - end.0), end.1 + 2.0 / 3.0 * (ctrl.1 - end.1));
                    cubic(&mut current, (x, y), c1, c2, end);
                    last_ctrl = Some(ctrl);
                    x = end.0;
                    y = end.1;
                }
                b'A' => {
                    let end = (ox + a[5], oy + a[6]);
                    arc(&mut current, (x, y), a[0], a[1], a[2], a[3] != 0.0, a[4] != 0.0, end);
                    x = end.0;
                    y = end.1;
                    last_ctrl = None;
                }
                _ => {}
            }
            last_cmd = if op == b'M' && rel { b'm' } else { cmd };
        }
        last_cmd = cmd;
    }
    finish(&mut current, &mut contours, &mut closed, false);
    (contours, closed)
}

fn cubic(out: &mut Contour, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)) {
    let length = ((p1.0 - p0.0).hypot(p1.1 - p0.1) + (p2.0 - p1.0).hypot(p2.1 - p1.1) + (p3.0 - p2.0).hypot(p3.1 - p2.1)).max(0.1);
    let steps = (length / 1.5).ceil().clamp(4.0, 64.0) as usize;
    for k in 1..=steps {
        let t = k as f32 / steps as f32;
        let u = 1.0 - t;
        let x = u * u * u * p0.0 + 3.0 * u * u * t * p1.0 + 3.0 * u * t * t * p2.0 + t * t * t * p3.0;
        let y = u * u * u * p0.1 + 3.0 * u * u * t * p1.1 + 3.0 * u * t * t * p2.1 + t * t * t * p3.1;
        out.push((x, y));
    }
}

/// SVG endpoint arc (F.6.5 of the spec) flattened into `out`.
#[allow(clippy::too_many_arguments)]
fn arc(out: &mut Contour, from: (f32, f32), rx: f32, ry: f32, rotation: f32, large: bool, sweep: bool, to: (f32, f32)) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx < 1e-4 || ry < 1e-4 || (from.0 - to.0).abs() + (from.1 - to.1).abs() < 1e-4 {
        out.push(to);
        return;
    }
    let (sin, cos) = rotation.to_radians().sin_cos();
    let dx = (from.0 - to.0) / 2.0;
    let dy = (from.1 - to.1) / 2.0;
    let x1 = cos * dx + sin * dy;
    let y1 = -sin * dx + cos * dy;
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut coef = (num / den).max(0.0).sqrt();
    if large == sweep {
        coef = -coef;
    }
    let cx1 = coef * rx * y1 / ry;
    let cy1 = -coef * ry * x1 / rx;
    let cx = cos * cx1 - sin * cy1 + (from.0 + to.0) / 2.0;
    let cy = sin * cx1 + cos * cy1 + (from.1 + to.1) / 2.0;
    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| {
        let a = (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
        a
    };
    let theta1 = angle(1.0, 0.0, (x1 - cx1) / rx, (y1 - cy1) / ry);
    let mut delta = angle((x1 - cx1) / rx, (y1 - cy1) / ry, (-x1 - cx1) / rx, (-y1 - cy1) / ry);
    if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    } else if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    }
    let steps = ((delta.abs() * rx.max(ry)) / 1.5).ceil().clamp(4.0, 96.0) as usize;
    for k in 1..=steps {
        let t = theta1 + delta * k as f32 / steps as f32;
        let (st, ct) = t.sin_cos();
        out.push((cos * rx * ct - sin * ry * st + cx, sin * rx * ct + cos * ry * st + cy));
    }
}

fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Contour {
    let steps = ((rx.max(ry) * 0.8).ceil() as usize).clamp(16, 128);
    (0..=steps)
        .map(|k| {
            let t = std::f32::consts::TAU * k as f32 / steps as f32;
            (cx + rx * t.cos(), cy + ry * t.sin())
        })
        .collect()
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32) -> Contour {
    let (rx, ry) = (rx.min(w / 2.0), ry.min(h / 2.0));
    if rx <= 0.0 || ry <= 0.0 {
        return vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)];
    }
    let mut out = Vec::new();
    let corner = |out: &mut Contour, cx: f32, cy: f32, start: f32| {
        for k in 0..=8 {
            let t = start + std::f32::consts::FRAC_PI_2 * k as f32 / 8.0;
            out.push((cx + rx * t.cos(), cy + ry * t.sin()));
        }
    };
    use std::f32::consts::PI;
    corner(&mut out, x + w - rx, y + ry, -PI / 2.0);
    corner(&mut out, x + w - rx, y + h - ry, 0.0);
    corner(&mut out, x + rx, y + h - ry, PI / 2.0);
    corner(&mut out, x + rx, y + ry, PI);
    out.push(out[0]);
    out
}

/// Paint state inherited down the tree.
#[derive(Clone)]
struct Paint {
    fill: Option<Color>,
    stroke: Option<Color>,
    stroke_width: f32,
    even_odd: bool,
    opacity: f32,
    fill_opacity: f32,
    stroke_opacity: f32,
    current: Color,
}

struct Canvas {
    w: usize,
    h: usize,
    /// premultiplied RGBA as f32
    px: Vec<[f32; 4]>,
}

impl Canvas {
    /// Fill `contours` (device px) with `color`, scanning 4 sub-rows per pixel.
    fn fill(&mut self, contours: &[Contour], color: Color, even_odd: bool) {
        if color.a == 0 {
            return;
        }
        let mut edges: Vec<(f32, f32, f32, f32, i32)> = Vec::new();
        let (mut min_y, mut max_y) = (f32::MAX, f32::MIN);
        for contour in contours {
            for w in contour.windows(2) {
                let ((x0, y0), (x1, y1)) = (w[0], w[1]);
                if (y0 - y1).abs() < 1e-6 {
                    continue;
                }
                let dir = if y1 > y0 { 1 } else { -1 };
                let (ya, yb, xa, xb) = if y0 < y1 { (y0, y1, x0, x1) } else { (y1, y0, x1, x0) };
                edges.push((ya, yb, xa, (xb - xa) / (yb - ya), dir));
                min_y = min_y.min(ya);
                max_y = max_y.max(yb);
            }
            // implicitly closed
            if let (Some(&(x0, y0)), Some(&(x1, y1))) = (contour.last(), contour.first()) {
                if (y0 - y1).abs() > 1e-6 {
                    let dir = if y1 > y0 { 1 } else { -1 };
                    let (ya, yb, xa, xb) = if y0 < y1 { (y0, y1, x0, x1) } else { (y1, y0, x1, x0) };
                    edges.push((ya, yb, xa, (xb - xa) / (yb - ya), dir));
                    min_y = min_y.min(ya);
                    max_y = max_y.max(yb);
                }
            }
        }
        if edges.is_empty() {
            return;
        }
        let a = color.a as f32 / 255.0;
        let rgb = [color.r as f32 * a, color.g as f32 * a, color.b as f32 * a, a];
        let y_start = (min_y.floor().max(0.0)) as usize;
        let y_end = (max_y.ceil().min(self.h as f32)) as usize;
        let mut row = vec![0.0f32; self.w + 2];
        let mut crossings: Vec<(f32, i32)> = Vec::new();
        for py in y_start..y_end {
            row.iter_mut().for_each(|c| *c = 0.0);
            for sub in 0..4 {
                let sy = py as f32 + (sub as f32 + 0.5) / 4.0;
                crossings.clear();
                for &(ya, yb, xa, slope, dir) in &edges {
                    if sy >= ya && sy < yb {
                        crossings.push((xa + (sy - ya) * slope, dir));
                    }
                }
                crossings.sort_by(|p, q| p.0.partial_cmp(&q.0).unwrap_or(std::cmp::Ordering::Equal));
                let mut winding = 0;
                for k in 0..crossings.len() {
                    winding += crossings[k].1;
                    let inside = if even_odd { (k + 1) % 2 == 1 } else { winding != 0 };
                    if inside && k + 1 < crossings.len() {
                        let (x0, x1) = (crossings[k].0.max(0.0), crossings[k + 1].0.min(self.w as f32));
                        if x1 > x0 {
                            add_span(&mut row, x0, x1, 0.25);
                        }
                    }
                }
            }
            for x in 0..self.w {
                let cov = row[x].min(1.0);
                if cov <= 0.0 {
                    continue;
                }
                let p = &mut self.px[py * self.w + x];
                let k = cov;
                for c in 0..4 {
                    p[c] = rgb[c] * k + p[c] * (1.0 - rgb[3] * k);
                }
            }
        }
    }
}

/// Add `weight` × the covered fraction of each pixel in [x0, x1) to `row`.
fn add_span(row: &mut [f32], x0: f32, x1: f32, weight: f32) {
    let (first, last) = (x0.floor() as usize, x1.floor() as usize);
    if first == last {
        row[first] += (x1 - x0) * weight;
        return;
    }
    row[first] += (first as f32 + 1.0 - x0) * weight;
    let end = last.min(row.len());
    for c in &mut row[first + 1..end] {
        *c += weight;
    }
    if last < row.len() {
        row[last] += (x1 - last as f32) * weight;
    }
}

/// Outline a polyline of width `w` as polygons: a quad per segment and a
/// round join at every vertex, all wound the same way.
fn stroke_polygons(points: &Contour, closed: bool, w: f32) -> Vec<Contour> {
    let half = w / 2.0;
    let mut out = Vec::new();
    let pts: Vec<(f32, f32)> = points.windows(2).filter(|p| (p[0].0 - p[1].0).abs() + (p[0].1 - p[1].1).abs() > 1e-5).map(|p| p[0]).chain(points.last().copied()).collect();
    if pts.len() < 2 {
        return out;
    }
    for s in pts.windows(2) {
        let ((x0, y0), (x1, y1)) = (s[0], s[1]);
        let len = (x1 - x0).hypot(y1 - y0);
        if len < 1e-5 {
            continue;
        }
        let (nx, ny) = (-(y1 - y0) / len * half, (x1 - x0) / len * half);
        out.push(vec![(x0 + nx, y0 + ny), (x1 + nx, y1 + ny), (x1 - nx, y1 - ny), (x0 - nx, y0 - ny), (x0 + nx, y0 + ny)]);
    }
    let joins = if closed { &pts[..] } else { &pts[..] };
    for &(x, y) in joins {
        out.push(ellipse(x, y, half, half));
    }
    // same orientation everywhere, so non-zero winding unions them
    for poly in &mut out {
        let area: f32 = poly.windows(2).map(|p| p[0].0 * p[1].1 - p[1].0 * p[0].1).sum();
        if area < 0.0 {
            poly.reverse();
        }
    }
    out
}

fn attr<'n>(node: &'n Node, name: &str) -> Option<&'n str> {
    // presentation attribute, overridden by the same property in style=""
    let from_style = node.attr("style").and_then(|style| {
        style.split(';').find_map(|decl| {
            let (k, v) = decl.split_once(':')?;
            (k.trim().eq_ignore_ascii_case(name)).then(|| v.trim())
        })
    });
    from_style.or_else(|| node.attr(name))
}

fn length(node: &Node, name: &str, base: f32) -> f32 {
    attr(node, name)
        .and_then(|v| {
            let v = v.trim();
            if let Some(p) = v.strip_suffix('%') {
                p.parse::<f32>().ok().map(|p| p / 100.0 * base)
            } else {
                v.trim_end_matches("px").parse::<f32>().ok()
            }
        })
        .unwrap_or(0.0)
}

struct Renderer<'d> {
    canvas: Canvas,
    root: &'d Node,
}

impl<'d> Renderer<'d> {
    /// Colour of a paint value; gradients and patterns by reference become
    /// their first stop colour.
    fn paint(&self, value: &str, current: Color) -> Option<Option<Color>> {
        let v = value.trim();
        if v.eq_ignore_ascii_case("none") || v.eq_ignore_ascii_case("transparent") {
            return Some(None);
        }
        if v.eq_ignore_ascii_case("currentcolor") {
            return Some(Some(current));
        }
        if let Some(id) = v.strip_prefix("url(").and_then(|r| r.split(')').next()) {
            let id = id.trim().trim_matches(|c| c == '"' || c == '\'').trim_start_matches('#');
            let fallback = v.split(')').nth(1).and_then(|f| parse_color(f.trim()));
            let stop = find_by_id(self.root, id).and_then(|grad| first_stop(grad, self.root));
            return Some(stop.or(fallback).or(Some(Color::BLACK)));
        }
        parse_color(v).map(Some)
    }

    fn draw(&mut self, node: &Node, transform: Affine, paint: &Paint) {
        if node.text.is_some() {
            return;
        }
        let tag = node.tag.as_str();
        if matches!(tag, "defs" | "clippath" | "mask" | "pattern" | "symbol" | "marker" | "title" | "desc" | "metadata" | "style" | "script" | "text" | "lineargradient" | "radialgradient" | "filter" | "foreignobject" | "image" | "use") {
            return;
        }
        if attr(node, "display") == Some("none") || attr(node, "visibility") == Some("hidden") {
            return;
        }
        let transform = match node.attr("transform") {
            Some(t) => parse_transform(t).then(transform),
            None => transform,
        };
        let mut paint = paint.clone();
        if let Some(c) = attr(node, "color").and_then(parse_color) {
            paint.current = c;
        }
        if let Some(v) = attr(node, "fill") {
            if let Some(p) = self.paint(v, paint.current) {
                paint.fill = p;
            }
        }
        if let Some(v) = attr(node, "stroke") {
            if let Some(p) = self.paint(v, paint.current) {
                paint.stroke = p;
            }
        }
        if let Some(w) = attr(node, "stroke-width").and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()) {
            paint.stroke_width = w;
        }
        if let Some(rule) = attr(node, "fill-rule") {
            paint.even_odd = rule.trim() == "evenodd";
        }
        let number = |name: &str| attr(node, name).and_then(|v| v.trim().parse::<f32>().ok());
        if let Some(o) = number("opacity") {
            paint.opacity *= o.clamp(0.0, 1.0);
        }
        if let Some(o) = number("fill-opacity") {
            paint.fill_opacity = o.clamp(0.0, 1.0);
        }
        if let Some(o) = number("stroke-opacity") {
            paint.stroke_opacity = o.clamp(0.0, 1.0);
        }

        let (contours, closed): (Vec<Contour>, Vec<bool>) = match tag {
            "path" => parse_path(node.attr("d").unwrap_or("")),
            "rect" => {
                let (w, h) = (length(node, "width", 0.0), length(node, "height", 0.0));
                if w <= 0.0 || h <= 0.0 {
                    return;
                }
                let rx = attr(node, "rx").and_then(|v| v.parse::<f32>().ok());
                let ry = attr(node, "ry").and_then(|v| v.parse::<f32>().ok());
                let (rx, ry) = (rx.or(ry).unwrap_or(0.0), ry.or(rx).unwrap_or(0.0));
                (vec![rounded_rect(length(node, "x", 0.0), length(node, "y", 0.0), w, h, rx, ry)], vec![true])
            }
            "circle" => {
                let r = length(node, "r", 0.0);
                (vec![ellipse(length(node, "cx", 0.0), length(node, "cy", 0.0), r, r)], vec![true])
            }
            "ellipse" => (vec![ellipse(length(node, "cx", 0.0), length(node, "cy", 0.0), length(node, "rx", 0.0), length(node, "ry", 0.0))], vec![true]),
            "line" => (
                vec![vec![(length(node, "x1", 0.0), length(node, "y1", 0.0)), (length(node, "x2", 0.0), length(node, "y2", 0.0))]],
                vec![false],
            ),
            "polyline" | "polygon" => {
                let n = numbers(node.attr("points").unwrap_or(""));
                let mut c: Contour = n.chunks_exact(2).map(|p| (p[0], p[1])).collect();
                if tag == "polygon" {
                    if let Some(&first) = c.first() {
                        c.push(first);
                    }
                }
                (vec![c], vec![tag == "polygon"])
            }
            "svg" | "g" | "a" | "switch" => {
                // a nested <svg> establishes a new viewport
                let inner = if tag == "svg" && !std::ptr::eq(node, self.root) { nested_viewport(node).then(transform) } else { transform };
                for child in &node.children {
                    self.draw(child, inner, &paint);
                }
                return;
            }
            _ => {
                for child in &node.children {
                    self.draw(child, transform, &paint);
                }
                return;
            }
        };
        let device: Vec<Contour> = contours.iter().map(|c| c.iter().map(|&(x, y)| transform.apply(x, y)).collect()).collect();
        let alpha = |c: Color, k: f32| Color::rgba(c.r, c.g, c.b, (c.a as f32 * k).round() as u8);
        if let Some(fill) = paint.fill {
            if tag != "line" {
                self.canvas.fill(&device, alpha(fill, paint.opacity * paint.fill_opacity), paint.even_odd);
            }
        }
        if let Some(stroke) = paint.stroke {
            let width = paint.stroke_width * transform.scale_factor();
            if width > 0.0 {
                let mut outline = Vec::new();
                for (contour, is_closed) in device.iter().zip(closed.iter()) {
                    outline.extend(stroke_polygons(contour, *is_closed, width.max(0.5)));
                }
                self.canvas.fill(&outline, alpha(stroke, paint.opacity * paint.stroke_opacity), false);
            }
        }
    }
}

fn nested_viewport(node: &Node) -> Affine {
    let (x, y) = (length(node, "x", 0.0), length(node, "y", 0.0));
    let place = Affine([1.0, 0.0, 0.0, 1.0, x, y]);
    match viewbox(node) {
        Some((vx, vy, vw, vh)) => {
            let (w, h) = (length(node, "width", vw), length(node, "height", vh));
            let (w, h) = (if w > 0.0 { w } else { vw }, if h > 0.0 { h } else { vh });
            let s = (w / vw).min(h / vh);
            Affine([s, 0.0, 0.0, s, -vx * s + (w - vw * s) / 2.0, -vy * s + (h - vh * s) / 2.0]).then(place)
        }
        None => place,
    }
}

fn viewbox(node: &Node) -> Option<(f32, f32, f32, f32)> {
    let n = numbers(node.attr("viewbox")?);
    (n.len() == 4 && n[2] > 0.0 && n[3] > 0.0).then(|| (n[0], n[1], n[2], n[3]))
}

fn find_by_id<'n>(node: &'n Node, id: &str) -> Option<&'n Node> {
    if node.attr("id") == Some(id) {
        return Some(node);
    }
    node.children.iter().find_map(|c| find_by_id(c, id))
}

fn first_stop(gradient: &Node, root: &Node) -> Option<Color> {
    let stop = gradient.children.iter().find(|c| c.tag == "stop");
    match stop {
        Some(stop) => attr(stop, "stop-color").and_then(parse_color),
        // a gradient may borrow its stops from another one
        None => {
            let href = gradient.attr("href").or_else(|| gradient.attr("xlink:href"))?;
            let other = find_by_id(root, href.trim_start_matches('#'))?;
            if std::ptr::eq(other, gradient) {
                return None;
            }
            first_stop(other, root)
        }
    }
}

/// The natural size of an `<svg>`: width/height attributes, else its viewBox, else 300×150.
pub fn natural_size(svg: &Node) -> (f32, f32) {
    let vb = viewbox(svg);
    let w = length(svg, "width", 0.0);
    let h = length(svg, "height", 0.0);
    match (w > 0.0, h > 0.0, vb) {
        (true, true, _) => (w, h),
        (true, false, Some((_, _, vw, vh))) => (w, w * vh / vw),
        (false, true, Some((_, _, vw, vh))) => (h * vw / vh, h),
        (false, false, Some((_, _, vw, vh))) => (vw, vh),
        _ => (if w > 0.0 { w } else { 300.0 }, if h > 0.0 { h } else { 150.0 }),
    }
}

/// Render an `<svg>` element to a bitmap of `width`×`height` device pixels.
/// `current` is what `currentColor` means (the text colour around it).
pub fn render_node(svg: &Node, width: usize, height: usize, current: Color) -> Bitmap {
    render_node_styled(svg, width, height, current, None, None)
}

/// `render_node` with the CSS `fill` / `stroke` the `<svg>` element got from
/// the page's stylesheet (octicons: `.octicon { fill: currentColor }`): they
/// replace the SVG defaults; attributes inside still win.
/// `render_node_styled`, remembered: the same icon (same markup, size and
/// colours) is drawn once per page thread, not on every layout pass. Pages
/// repeat a few icons hundreds of times and lay out several times.
pub fn render_node_cached(svg: &Node, width: usize, height: usize, natural: (f32, f32), current: Color, fill: Option<Option<Color>>, stroke: Option<Option<Color>>) -> std::sync::Arc<Bitmap> {
    use std::hash::{Hash, Hasher};
    fn walk(n: &Node, h: &mut std::collections::hash_map::DefaultHasher) {
        n.tag.hash(h);
        n.attrs.hash(h);
        n.text.hash(h);
        n.children.len().hash(h);
        for c in &n.children {
            walk(c, h);
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    walk(svg, &mut h);
    (width, height, natural.0.to_bits(), natural.1.to_bits()).hash(&mut h);
    let rgba = |c: Color| (c.r, c.g, c.b, c.a);
    rgba(current).hash(&mut h);
    fill.map(|f| f.map(rgba)).hash(&mut h);
    stroke.map(|s| s.map(rgba)).hash(&mut h);
    let key = h.finish();
    thread_local! {
        static DRAWN: std::cell::RefCell<std::collections::HashMap<u64, std::sync::Arc<Bitmap>>> = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    if let Some(hit) = DRAWN.with(|d| d.borrow().get(&key).cloned()) {
        return hit;
    }
    let mut bitmap = render_node_styled(svg, width, height, current, fill, stroke);
    bitmap.natural = Some(natural);
    let bitmap = std::sync::Arc::new(bitmap);
    DRAWN.with(|d| {
        let mut d = d.borrow_mut();
        // a bound, so a page of ever-new SVG cannot grow it without end
        if d.len() > 4096 {
            d.clear();
        }
        d.insert(key, bitmap.clone());
    });
    bitmap
}

pub fn render_node_styled(svg: &Node, width: usize, height: usize, current: Color, fill: Option<Option<Color>>, stroke: Option<Option<Color>>) -> Bitmap {
    let (width, height) = (width.clamp(1, 2048), height.clamp(1, 2048));
    let mut renderer = Renderer { canvas: Canvas { w: width, h: height, px: vec![[0.0; 4]; width * height] }, root: svg };
    let view = match viewbox(svg) {
        Some((vx, vy, vw, vh)) => {
            // preserveAspectRatio: xMidYMid meet
            let s = (width as f32 / vw).min(height as f32 / vh);
            Affine([s, 0.0, 0.0, s, -vx * s + (width as f32 - vw * s) / 2.0, -vy * s + (height as f32 - vh * s) / 2.0])
        }
        None => {
            let (nw, nh) = natural_size(svg);
            Affine([width as f32 / nw, 0.0, 0.0, height as f32 / nh, 0.0, 0.0])
        }
    };
    let paint = Paint {
        fill: fill.unwrap_or(Some(Color::BLACK)),
        stroke: stroke.unwrap_or(None),
        stroke_width: 1.0,
        even_odd: false,
        opacity: 1.0,
        fill_opacity: 1.0,
        stroke_opacity: 1.0,
        current,
    };
    renderer.draw(svg, view, &paint);
    let rgba = renderer
        .canvas
        .px
        .iter()
        .flat_map(|p| [(p[0].round().clamp(0.0, 255.0)) as u8, (p[1].round().clamp(0.0, 255.0)) as u8, (p[2].round().clamp(0.0, 255.0)) as u8, ((p[3] * 255.0).round().clamp(0.0, 255.0)) as u8])
        .collect();
    Bitmap::from_premultiplied(width, height, rgba)
}

/// An SVG file → bitmap. Rendered at three times its natural size (capped)
/// so it stays sharp when scaled down; the bitmap remembers the natural size.
pub fn decode(bytes: &[u8]) -> Option<Bitmap> {
    let text = std::str::from_utf8(bytes).ok()?;
    let document = dom::parse_document(text);
    let svg = document.find_first("svg")?;
    let (nw, nh) = natural_size(svg);
    let scale = (512.0 / nw.max(nh)).min(3.0).max(0.05);
    let mut bitmap = render_node(svg, (nw * scale).round() as usize, (nh * scale).round() as usize, Color::BLACK);
    bitmap.natural = Some((nw, nh));
    Some(bitmap)
}

/// Does this look like SVG markup?
pub fn sniff(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with("<svg") || ((text.starts_with("<?xml") || text.starts_with("<!--") || text.starts_with("<!doctype svg")) && text.contains("<svg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(b: &Bitmap, x: usize, y: usize) -> [u8; 4] {
        let i = (y * b.width + x) * 4;
        [b.rgba[i], b.rgba[i + 1], b.rgba[i + 2], b.rgba[i + 3]]
    }

    fn svg(markup: &str, w: usize, h: usize) -> Bitmap {
        let doc = dom::parse_document(markup);
        render_node(doc.find_first("svg").expect("svg"), w, h, Color::rgb(0, 0, 255))
    }

    #[test]
    fn numbers_split_like_svg() {
        assert_eq!(numbers("1.5.5-2e1,3"), vec![1.5, 0.5, -20.0, 3.0]);
    }

    #[test]
    fn fills_shapes_with_viewbox_scaling() {
        let b = svg(r##"<svg viewBox="0 0 10 10"><rect x="0" y="0" width="5" height="10" fill="#ff0000"/></svg>"##, 20, 20);
        assert_eq!(pixel(&b, 4, 10), [255, 0, 0, 255], "left half red");
        assert_eq!(pixel(&b, 15, 10)[3], 0, "right half empty");
        let c = svg(r#"<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="10" fill="currentColor"/></svg>"#, 24, 24);
        assert_eq!(pixel(&c, 12, 12), [0, 0, 255, 255], "currentColor");
        assert_eq!(pixel(&c, 0, 0)[3], 0);
        let edge = pixel(&c, 2, 12)[3];
        assert!(edge > 0 && edge < 255, "anti-aliased edge: {edge}");
    }

    #[test]
    fn paths_curves_arcs_and_even_odd() {
        // a ring: outer and inner circle with evenodd leaves the centre empty
        let ring = svg(
            r#"<svg viewBox="0 0 20 20"><path fill-rule="evenodd" d="M10 0a10 10 0 1 1 0 20a10 10 0 1 1 0-20zM10 5a5 5 0 1 0 0 10a5 5 0 1 0 0-10z"/></svg>"#,
            40,
            40,
        );
        assert_eq!(pixel(&ring, 20, 20)[3], 0, "hole");
        assert_eq!(pixel(&ring, 20, 4)[3], 255, "ring");
        let curve = svg(r#"<svg viewBox="0 0 10 10"><path d="M0 10 Q5 -10 10 10 Z" fill="black"/></svg>"#, 20, 20);
        assert_eq!(pixel(&curve, 10, 14)[3], 255);
    }

    #[test]
    fn strokes_transforms_and_gradient_fallback() {
        let s = svg(r#"<svg viewBox="0 0 10 10"><line x1="0" y1="5" x2="10" y2="5" stroke="black" stroke-width="2"/></svg>"#, 10, 10);
        assert_eq!(pixel(&s, 5, 4)[3], 255, "stroked line");
        assert_eq!(pixel(&s, 5, 1)[3], 0);
        let t = svg(r#"<svg viewBox="0 0 10 10"><g transform="translate(5 0)"><rect width="5" height="10" fill="lime"/></g></svg>"#, 10, 10);
        assert_eq!(pixel(&t, 2, 5)[3], 0);
        assert_eq!(pixel(&t, 7, 5), [0, 255, 0, 255]);
        let g = svg(
            r##"<svg viewBox="0 0 10 10"><defs><linearGradient id="g"><stop offset="0" stop-color="#00ff00"/><stop offset="1" stop-color="#000"/></linearGradient></defs><rect width="10" height="10" fill="url(#g)"/></svg>"##,
            10,
            10,
        );
        assert_eq!(pixel(&g, 5, 5), [0, 255, 0, 255]);
    }

    #[test]
    fn decodes_files_at_natural_size() {
        let bytes = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><path d="M2 2h20v20H2z"/></svg>"#;
        assert!(sniff(bytes));
        let b = decode(bytes).expect("svg");
        assert_eq!(b.natural, Some((24.0, 24.0)));
        assert!(b.width >= 48, "rendered above natural size for sharpness");
        assert!(!sniff(b"\x89PNG"));
    }
}
