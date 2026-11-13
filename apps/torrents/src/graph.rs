//! Live line graphs drawn with cairo. Colours come only from the theme tokens:
//! the first series is `accent`, the second `mix(accent, danger, 0.55)` and
//! dashed (so the pair never relies on colour alone). Text stays in text tokens.

use crate::theme::{self, Rgb};
use crate::{live, prefs, widgets};
use gtk::cairo;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Second,
}

impl Tone {
    pub fn rgb(self) -> Rgb {
        let p = theme::palette();
        let accent = Rgb::hex(&p.accent);
        match self {
            Tone::Accent => accent,
            Tone::Second => accent.mix(Rgb::hex(&p.danger), 0.55),
        }
    }

    fn class(self) -> &'static str {
        match self {
            Tone::Accent => "tone-accent",
            Tone::Second => "tone-second",
        }
    }
}

#[derive(Clone)]
pub struct Series {
    pub key: String,
    pub label: String,
    pub tone: Tone,
    pub dashed: bool,
}

impl Series {
    pub fn new(key: impl Into<String>, label: &str, tone: Tone) -> Series {
        Series { key: key.into(), label: label.into(), tone, dashed: tone == Tone::Second }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Scale {
    /// 0 to a rounded-up maximum of what's on screen, never below `floor`.
    Auto { floor: f64 },
}

pub fn nice_ceiling(v: f64) -> f64 {
    if v <= 0.0 {
        return 1.0;
    }
    let exp = 10f64.powf(v.log10().floor());
    let f = v / exp;
    let n = if f <= 1.0 {
        1.0
    } else if f <= 2.0 {
        2.0
    } else if f <= 2.5 {
        2.5
    } else if f <= 5.0 {
        5.0
    } else {
        10.0
    };
    n * exp
}

fn scale_max(scale: Scale, series: &[Vec<f64>]) -> f64 {
    match scale {
        Scale::Auto { floor } => {
            let m = series.iter().flatten().copied().fold(0.0, f64::max);
            nice_ceiling((m * 1.1).max(floor))
        }
    }
}

fn set(cr: &cairo::Context, c: Rgb, a: f64) {
    cr.set_source_rgba(c.0, c.1, c.2, a);
}

/// Draw series right-aligned: the newest sample sits on the right edge.
fn draw_lines(cr: &cairo::Context, w: f64, h: f64, cap: usize, data: &[(Vec<f64>, Tone, bool)], max: f64, width: f64) {
    let step = w / (cap.max(2) - 1) as f64;
    let y = |v: f64| (h - 1.0) - (v / max).clamp(0.0, 1.0) * (h - 3.0);
    // Fills first, lines on top; the first series is drawn last so it stays in front.
    for (i, (values, tone, dashed)) in data.iter().enumerate().rev() {
        if values.len() < 2 {
            continue;
        }
        let n = values.len();
        let x0 = w - (n - 1) as f64 * step;
        let c = tone.rgb();
        if !*dashed {
            cr.move_to(x0, h);
            for (j, v) in values.iter().enumerate() {
                cr.line_to(x0 + j as f64 * step, y(*v));
            }
            cr.line_to(w, h);
            cr.close_path();
            let grad = cairo::LinearGradient::new(0.0, 0.0, 0.0, h);
            let top = if i == 0 { 0.26 } else { 0.14 };
            grad.add_color_stop_rgba(0.0, c.0, c.1, c.2, top);
            grad.add_color_stop_rgba(1.0, c.0, c.1, c.2, 0.02);
            let _ = cr.set_source(&grad);
            let _ = cr.fill();
        }
        for (j, v) in values.iter().enumerate() {
            let (px, py) = (x0 + j as f64 * step, y(*v));
            if j == 0 { cr.move_to(px, py) } else { cr.line_to(px, py) }
        }
        set(cr, c, 1.0);
        cr.set_line_width(width);
        cr.set_line_join(cairo::LineJoin::Round);
        cr.set_line_cap(cairo::LineCap::Round);
        if *dashed {
            cr.set_dash(&[5.0, 3.5], 0.0);
        }
        let _ = cr.stroke();
        cr.set_dash(&[], 0.0);
    }
}

pub struct Graph {
    pub root: gtk::Box,
}

/// A full graph: legend with live values, the plot with a grid and a hover
/// crosshair, and a time axis.
pub fn graph(series: Vec<Series>, scale: Scale, fmt: fn(f64) -> String, height: i32) -> Graph {
    let root = widgets::vbox(6);
    root.add_css_class("graph");

    // Legend: a single series needs none (the card title names it), but its value still shows.
    let legend = widgets::hbox(16);
    legend.add_css_class("graph-legend");
    let mut values = Vec::new();
    for s in &series {
        let item = widgets::hbox(6);
        if series.len() > 1 {
            let sw = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            sw.add_css_class("legend-swatch");
            sw.add_css_class(s.tone.class());
            if s.dashed {
                sw.add_css_class("dashed");
            }
            sw.set_valign(gtk::Align::Center);
            item.append(&sw);
            item.append(&widgets::label(&s.label, "legend-label"));
        }
        let v = widgets::label("–", "legend-value");
        v.add_css_class("tnum");
        item.append(&v);
        values.push(v);
        legend.append(&item);
    }
    // One series: its card header already shows the value.
    legend.set_visible(series.len() > 1);
    root.append(&legend);

    let area = gtk::DrawingArea::new();
    area.set_content_height(height);
    area.set_hexpand(true);
    let hover: Rc<Cell<Option<f64>>> = Rc::new(Cell::new(None));

    let overlay = gtk::Overlay::new();
    overlay.add_css_class("graph-plot");
    overlay.set_child(Some(&area));
    let max_label = widgets::label("", "graph-axis");
    max_label.add_css_class("tnum");
    max_label.set_halign(gtk::Align::Start);
    max_label.set_valign(gtk::Align::Start);
    max_label.set_can_target(false);
    overlay.add_overlay(&max_label);
    let tip = widgets::label("", "graph-tip");
    tip.add_css_class("tnum");
    tip.set_halign(gtk::Align::Start);
    tip.set_valign(gtk::Align::Start);
    tip.set_visible(false);
    tip.set_can_target(false);
    overlay.add_overlay(&tip);
    root.append(&overlay);

    let axis = widgets::hbox(0);
    let ago = widgets::label("", "graph-axis");
    ago.set_hexpand(true);
    axis.append(&ago);
    axis.append(&widgets::label("now", "graph-axis"));
    root.append(&axis);

    let series = Rc::new(series);
    let max = Rc::new(Cell::new(1.0f64));

    // Labels are updated here, outside drawing, so the plot never relayouts mid-frame.
    let update: Rc<dyn Fn()> = {
        let series = series.clone();
        let hover = hover.clone();
        let max = max.clone();
        let area = area.clone();
        Rc::new(move || {
            let raw: Vec<Vec<f64>> = series.iter().map(|s| live::history(&s.key)).collect();
            let m = scale_max(scale, &raw);
            max.set(m);
            max_label.set_text(&fmt(m));
            let w = area.width() as f64;
            let cap = live::capacity();
            let Some(hx) = hover.get().filter(|_| w > 0.0) else {
                tip.set_visible(false);
                return;
            };
            let step = w / (cap.max(2) - 1) as f64;
            let back = ((w - hx) / step).round().max(0.0) as usize;
            let x = w - back as f64 * step;
            let mut lines = Vec::new();
            for (s, values) in series.iter().zip(&raw) {
                if back >= values.len() {
                    continue;
                }
                let v = values[values.len() - 1 - back];
                lines.push(if series.len() == 1 { fmt(v) } else { format!("{}  {}", s.label, fmt(v)) });
            }
            if lines.is_empty() {
                tip.set_visible(false);
                return;
            }
            let secs = back as f64 * prefs::get().interval_ms as f64 / 1000.0;
            lines.push(if back == 0 { "now".into() } else { format!("{} ago", crate::fmt::duration(secs)) });
            tip.set_text(&lines.join("\n"));
            tip.set_visible(true);
            let tw = tip.width().max(90) as f64;
            let left = if x + 12.0 + tw > w { x - 12.0 - tw } else { x + 12.0 };
            tip.set_margin_start(left.max(0.0) as i32);
            tip.set_margin_top(6);
        })
    };

    {
        let series = series.clone();
        let hover = hover.clone();
        let max = max.clone();
        area.set_draw_func(move |_, cr, w, h| {
            let (w, h) = (w as f64, h as f64);
            let p = theme::palette();
            let border = Rgb::hex(&p.border);
            cr.set_line_width(1.0);
            for i in 1..4 {
                let gy = (h * i as f64 / 4.0).round() + 0.5;
                cr.move_to(0.0, gy);
                cr.line_to(w, gy);
            }
            set(cr, border, 0.45);
            let _ = cr.stroke();
            cr.move_to(0.0, h - 0.5);
            cr.line_to(w, h - 0.5);
            set(cr, border, 0.8);
            let _ = cr.stroke();

            let data: Vec<(Vec<f64>, Tone, bool)> = series.iter().map(|s| (live::history(&s.key), s.tone, s.dashed)).collect();
            let max = max.get();
            let cap = live::capacity();
            draw_lines(cr, w, h, cap, &data, max, 2.0);

            let Some(hx) = hover.get() else { return };
            let step = w / (cap.max(2) - 1) as f64;
            let back = ((w - hx) / step).round().max(0.0) as usize;
            let x = w - back as f64 * step;
            cr.move_to(x.round() + 0.5, 0.0);
            cr.line_to(x.round() + 0.5, h);
            set(cr, Rgb::hex(&p.text), 0.3);
            let _ = cr.stroke();
            for (values, tone, _) in &data {
                if back >= values.len() {
                    continue;
                }
                let v = values[values.len() - 1 - back];
                let y = (h - 1.0) - (v / max).clamp(0.0, 1.0) * (h - 3.0);
                cr.arc(x, y, 4.5, 0.0, std::f64::consts::TAU);
                set(cr, Rgb::hex(&p.bg), 1.0);
                let _ = cr.fill();
                cr.arc(x, y, 3.0, 0.0, std::f64::consts::TAU);
                set(cr, tone.rgb(), 1.0);
                let _ = cr.fill();
            }
        });
    }
    let motion = gtk::EventControllerMotion::new();
    {
        let hover = hover.clone();
        let a = area.clone();
        let update = update.clone();
        motion.connect_motion(move |_, x, _| {
            hover.set(Some(x));
            update();
            a.queue_draw();
        });
    }
    {
        let hover = hover.clone();
        let a = area.clone();
        let update = update.clone();
        motion.connect_leave(move |_| {
            hover.set(None);
            update();
            a.queue_draw();
        });
    }
    area.add_controller(motion);

    {
        let a = area.clone();
        let values = values.clone();
        let series = series.clone();
        live::on_tick(&area, move |_| {
            for (label, s) in values.iter().zip(series.iter()) {
                if let Some(v) = live::history(&s.key).last() {
                    label.set_text(&fmt(*v));
                }
            }
            let secs = prefs::get().history_secs as f64;
            ago.set_text(&format!("{} ago", crate::fmt::duration(secs)));
            update();
            a.queue_draw();
        });
    }
    Graph { root }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_scale_up() {
        assert_eq!(nice_ceiling(0.7), 1.0);
        assert_eq!(nice_ceiling(3.0), 5.0);
        assert_eq!(nice_ceiling(2.2), 2.5);
        assert_eq!(nice_ceiling(1200.0), 2000.0);
    }
}
