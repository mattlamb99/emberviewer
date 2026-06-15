//! Small UI pieces shared by the desktop app and the wasm browser client:
//! vertical meters and the function invocation form. Kept free of `Session` so
//! both front-ends can reuse them.

use std::collections::HashMap;

use ember_proto::glow::{self, Value};

use crate::model::{format_value, Entry, FunctionInfo, InvocationOutcome};
use crate::net::NetCommand;

// ---------------------------------------------------------------------------
// Safety lock
// ---------------------------------------------------------------------------

/// How long (egui seconds) the padlock flashes after a blocked interaction.
pub const LOCK_FLASH_SECS: f64 = 0.7;

/// Render `content` greyed-out and inert when `!enabled`. Returns the content's
/// value and, when disabled, whether the user clicked the locked region - so the
/// caller can flash the padlock to explain why nothing happened.
pub fn lockable<R>(
    ui: &mut egui::Ui,
    enabled: bool,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> (R, bool) {
    let inner = ui.add_enabled_ui(enabled, content);
    let blocked = !enabled
        && ui
            .interact(
                inner.response.rect,
                inner.response.id.with("lock-overlay"),
                egui::Sense::click(),
            )
            .clicked();
    (inner.inner, blocked)
}

/// A padlock toggle button: 🔒 Locked / 🔓 Armed, flips `locked` on click, and
/// flashes a red fill while `now < flash_until`. Requests repaints while flashing.
pub fn lock_toggle(ui: &mut egui::Ui, locked: &mut bool, now: f64, flash_until: f64) {
    let (glyph, word, color) = if *locked {
        ('\u{1F512}', "Locked", egui::Color32::from_rgb(210, 120, 60))
    } else {
        ('\u{1F513}', "Armed", egui::Color32::from_rgb(80, 175, 100))
    };
    let mut btn = egui::Button::new(egui::RichText::new(format!("{glyph} {word}")).color(color));
    if now < flash_until {
        // Fade a red fill out over the flash window (pulses the eye to the lock).
        let frac = ((flash_until - now) / LOCK_FLASH_SECS).clamp(0.0, 1.0) as f32;
        btn = btn.fill(egui::Color32::from_rgb(210, 70, 60).gamma_multiply(frac));
        ui.ctx().request_repaint();
    }
    let tip = if *locked {
        "Controls are locked against accidental changes - click to arm"
    } else {
        "Editing enabled - click to lock"
    };
    if ui.add(btn).on_hover_text(tip).clicked() {
        *locked = !*locked;
    }
}

// ---------------------------------------------------------------------------
// Meters
// ---------------------------------------------------------------------------

/// A parameter's value as an `f64`, if it is numeric.
pub fn value_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Real(r) => Some(r.to_f64()),
        _ => None,
    }
}

/// Whether a parameter can be shown as a meter: a numeric value that is not an
/// enumeration (enum values are integers with labels, not levels) or a trigger.
/// Booleans/strings/octets are already non-numeric.
pub fn is_meterable(entry: &Entry) -> bool {
    entry.enum_entries.is_empty()
        && entry.param_type != Some(glow::parameter_type::TRIGGER)
        && entry.value.as_ref().and_then(value_f64).is_some()
}

/// The meter range for an entry: explicit min/max if present, else an
/// auto-tracked range that expands to fit observed values.
pub fn meter_range(entry: &Entry, tracked: &mut HashMap<Vec<u32>, (f64, f64)>) -> (f64, f64) {
    if let (Some(lo), Some(hi)) = (
        entry.minimum.as_ref().and_then(value_f64),
        entry.maximum.as_ref().and_then(value_f64),
    ) {
        if hi > lo {
            return (lo, hi);
        }
    }
    let v = entry.value.as_ref().and_then(value_f64).unwrap_or(0.0);
    let range = tracked
        .entry(entry.path.clone())
        .or_insert((v - 0.5, v + 0.5));
    range.0 = range.0.min(v);
    range.1 = range.1.max(v);
    if range.1 - range.0 < 1e-6 {
        range.1 = range.0 + 1.0;
    }
    *range
}

/// A meter's numeric readout: the value scaled by `factor` with the format unit,
/// so it matches the displayed (not raw) value.
pub fn meter_readout(entry: &Entry, v: f64) -> String {
    let factor = entry.factor.unwrap_or(1).max(1) as f64;
    format!("{:.2}{}", v / factor, format_suffix(entry))
}

// Tri-colour meter zones (fraction of full scale): green up to GREEN_TOP, amber
// to AMBER_TOP, red above - a mostly-green bar with a caution band and a small
// red headroom band at the top, the usual level-meter convention.
const MTR_GREEN_TOP: f32 = 0.75;
const MTR_AMBER_TOP: f32 = 0.90;
const MTR_GREEN: egui::Color32 = egui::Color32::from_rgb(40, 170, 80);
const MTR_AMBER: egui::Color32 = egui::Color32::from_rgb(210, 170, 30);
const MTR_RED: egui::Color32 = egui::Color32::from_rgb(210, 70, 60);

/// Draw a vertical bar-graph meter filling `width` × `height`, with the lit
/// portion coloured by zone (green / amber / red from bottom to top).
pub fn draw_vmeter(
    ui: &mut egui::Ui,
    value: Option<f64>,
    range: (f64, f64),
    width: f32,
    height: f32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
    if let Some(v) = value {
        let (min, max) = range;
        let frac = (((v - min) / (max - min)) as f32).clamp(0.0, 1.0);
        let bottom = rect.max.y - 1.0;
        let inner_h = rect.height() - 2.0;
        let left = rect.min.x + 1.0;
        let right = rect.max.x - 1.0;
        // Fill the segment [lo, hi] (fractions of full scale) in `color`.
        let seg = |lo: f32, hi: f32, color: egui::Color32| {
            if hi > lo {
                let r = egui::Rect::from_min_max(
                    egui::pos2(left, bottom - inner_h * hi),
                    egui::pos2(right, bottom - inner_h * lo),
                );
                painter.rect_filled(r, 0.0, color);
            }
        };
        seg(0.0, frac.min(MTR_GREEN_TOP), MTR_GREEN);
        if frac > MTR_GREEN_TOP {
            seg(MTR_GREEN_TOP, frac.min(MTR_AMBER_TOP), MTR_AMBER);
        }
        if frac > MTR_AMBER_TOP {
            seg(MTR_AMBER_TOP, frac, MTR_RED);
        }
    }
    resp
}

/// Draw a round indicator "light" filling `width` × `height`: green for
/// `Some(true)`, red for `Some(false)`, dim grey when the value is unknown
/// (`None`). The lit colours match the meter's green/red so the two pop-outs read
/// as one family.
// Only the desktop pop-out uses this; the wasm client has no pop-out windows.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub fn draw_indicator(
    ui: &mut egui::Ui,
    on: Option<bool>,
    width: f32,
    height: f32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let center = rect.center();
    let radius = (width.min(height) * 0.5 - 4.0).max(4.0);
    let fill = match on {
        Some(true) => MTR_GREEN,
        Some(false) => MTR_RED,
        None => egui::Color32::from_gray(80),
    };
    let painter = ui.painter();
    // A darker socket ring lifts the lit lens off the panel background.
    painter.circle_filled(center, radius + 2.0, ui.visuals().extreme_bg_color);
    painter.circle_filled(center, radius, fill);
    // A translucent top-left highlight reads as a glossy lens.
    painter.circle_filled(
        center - egui::vec2(radius * 0.3, radius * 0.3),
        radius * 0.3,
        egui::Color32::from_white_alpha(70),
    );
    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(1.0, egui::Color32::from_black_alpha(90)),
    );
    resp
}

// ---------------------------------------------------------------------------
// Value display (factor / format applied)
// ---------------------------------------------------------------------------

/// The literal suffix after a printf conversion in a parameter's `format`
/// (e.g. `"%d dB"` → `" dB"`), used as a unit on values/sliders.
pub fn format_suffix(entry: &Entry) -> String {
    let Some(fmt) = &entry.format else {
        return String::new();
    };
    if let Some(pct) = fmt.find('%') {
        let rest = &fmt[pct + 1..];
        if let Some(conv) = rest.find(|c: char| "diouxXeEfFgGsc".contains(c)) {
            return rest[conv + 1..].to_string();
        }
    }
    String::new()
}

/// Human-readable value: enum label, or factor/format-applied number, else raw.
pub fn display_value(entry: &Entry, v: &Value) -> String {
    match v {
        Value::Integer(i) => {
            if let Some(lbl) = entry.enum_label(*i) {
                lbl.to_string()
            } else {
                let factor = entry.factor.unwrap_or(1).max(1);
                if factor != 1 {
                    format!("{}{}", *i as f64 / factor as f64, format_suffix(entry))
                } else if entry.format.is_some() {
                    format!("{i}{}", format_suffix(entry))
                } else {
                    i.to_string()
                }
            }
        }
        Value::Real(r) if entry.format.is_some() => {
            format!("{}{}", r.to_f64(), format_suffix(entry))
        }
        _ => format_value(v),
    }
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

/// Short name for a parameter type id.
pub fn ptype_name(ptype: i32) -> &'static str {
    use glow::parameter_type as pt;
    match ptype {
        x if x == pt::INTEGER => "int",
        x if x == pt::REAL => "real",
        x if x == pt::STRING => "string",
        x if x == pt::BOOLEAN => "bool",
        x if x == pt::ENUM => "enum",
        x if x == pt::OCTETS => "octets",
        _ => "?",
    }
}

/// Parse a user-entered string into a `Value` of the given parameter type.
pub fn parse_value(s: &str, ptype: i32) -> Value {
    use glow::parameter_type as pt;
    let t = s.trim();
    match ptype {
        x if x == pt::INTEGER || x == pt::ENUM => Value::Integer(t.parse().unwrap_or(0)),
        x if x == pt::REAL => Value::Real(t.parse::<f64>().unwrap_or(0.0).into()),
        x if x == pt::BOOLEAN => Value::Boolean(matches!(
            t.to_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        )),
        _ => Value::String(s.to_string()),
    }
}

/// Render a function's argument form, an Invoke button, and the last result.
/// State lives outside (per-front-end) rather than in a `Session`.
#[allow(clippy::too_many_arguments)]
pub fn render_function(
    ui: &mut egui::Ui,
    entry: &Entry,
    f: &FunctionInfo,
    func_inputs: &mut HashMap<(Vec<u32>, usize), String>,
    invocations: &mut HashMap<Vec<u32>, i32>,
    next_invocation_id: &mut i32,
    invocation_results: &HashMap<i32, InvocationOutcome>,
    commands: &mut Vec<NetCommand>,
) {
    let path = entry.path.clone();
    if f.args.is_empty() {
        ui.weak("no arguments");
    }
    for (i, arg) in f.args.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("{} ({})", arg.name, ptype_name(arg.ptype)));
            let buf = func_inputs.entry((path.clone(), i)).or_default();
            ui.add(egui::TextEdit::singleline(buf).desired_width(120.0));
        });
    }
    if ui.button("Invoke").clicked() {
        let args: Vec<Value> = f
            .args
            .iter()
            .enumerate()
            .map(|(i, arg)| {
                let s = func_inputs
                    .get(&(path.clone(), i))
                    .cloned()
                    .unwrap_or_default();
                parse_value(&s, arg.ptype)
            })
            .collect();
        let id = *next_invocation_id;
        *next_invocation_id += 1;
        invocations.insert(path.clone(), id);
        commands.push(NetCommand::Invoke {
            path: path.clone(),
            invocation_id: id,
            args,
        });
    }
    if let Some(id) = invocations.get(&path) {
        if let Some(outcome) = invocation_results.get(id) {
            let names: Vec<String> = if f.result.len() == outcome.values.len() {
                f.result
                    .iter()
                    .zip(&outcome.values)
                    .map(|(slot, v)| format!("{}={}", slot.name, format_value(v)))
                    .collect()
            } else {
                outcome.values.iter().map(format_value).collect()
            };
            let status = if outcome.success { "OK" } else { "FAILED" };
            ui.colored_label(
                if outcome.success {
                    egui::Color32::from_rgb(40, 160, 80)
                } else {
                    egui::Color32::from_rgb(200, 60, 60)
                },
                format!("Result {status}: {}", names.join(", ")),
            );
        }
    }
}
