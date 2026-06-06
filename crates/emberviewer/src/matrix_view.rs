//! The matrix crosspoint grid — shared by the desktop app and the wasm browser
//! client. Targets-on-top puts targets across the columns and sources down the
//! rows (swap with `targets_on_top = false`). Returns `Some((is_target, signal))`
//! when a row/column header is clicked, so the caller can show that signal's
//! parameters.

use ember_proto::glow;

use crate::model::{Entry, MatrixInfo};
use crate::net::NetCommand;

pub fn render_matrix(
    ui: &mut egui::Ui,
    entry: &Entry,
    m: &MatrixInfo,
    targets_on_top: bool,
    commands: &mut Vec<NetCommand>,
) -> Option<(bool, u32)> {
    let mut header_clicked: Option<(bool, u32)> = None;
    let path = entry.path.clone();
    let kind = match m.mtype {
        x if x == glow::matrix_type::ONE_TO_N => "1:N",
        x if x == glow::matrix_type::ONE_TO_ONE => "1:1",
        x if x == glow::matrix_type::N_TO_N => "N:N",
        _ => "?",
    };
    ui.label(format!(
        "Matrix {}×{} ({kind})",
        m.target_count, m.source_count
    ));
    let (col_letter, row_letter) = if targets_on_top {
        ("targets", "sources")
    } else {
        ("sources", "targets")
    };
    ui.label(
        egui::RichText::new(format!(
            "columns (top) = {col_letter}, rows (left) = {row_letter}"
        ))
        .small()
        .weak(),
    );

    // Signal sets, augmented with any signals referenced by connections so a
    // crosspoint always has a visible cell even if targets/sources were sparse.
    let mut target_set: std::collections::BTreeSet<u32> = m.targets.iter().copied().collect();
    let mut source_set: std::collections::BTreeSet<u32> = m.sources.iter().copied().collect();
    for (t, srcs) in &m.connections {
        target_set.insert(*t);
        source_set.extend(srcs.iter().copied());
    }
    let targets_v: Vec<u32> = target_set.into_iter().collect();
    let sources_v: Vec<u32> = source_set.into_iter().collect();

    // Columns/rows iterate the actual signal numbers (sparse on real devices).
    let (col_signals, row_signals): (&[u32], &[u32]) = if targets_on_top {
        (&targets_v, &sources_v)
    } else {
        (&sources_v, &targets_v)
    };
    let (col_labels, row_labels) = if targets_on_top {
        (&m.target_labels, &m.source_labels)
    } else {
        (&m.source_labels, &m.target_labels)
    };
    let col_kind = if targets_on_top { "target" } else { "source" };
    let row_kind = if targets_on_top { "source" } else { "target" };

    let dark = ui.visuals().dark_mode;
    let (light_row, dark_row) = if dark {
        (egui::Color32::from_gray(40), egui::Color32::from_gray(72))
    } else {
        (egui::Color32::from_gray(238), egui::Color32::from_gray(212))
    };
    // Connected crosspoints get a darker, saturated burnt-orange so they stand
    // out more strongly than the muted row-selection tint.
    let crosspoint = egui::Color32::from_rgb(150, 74, 16);

    const CELL: f32 = 18.0;
    #[allow(unused)]
    let draw_label = |ui: &mut egui::Ui, text: &str, strong: bool| -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(CELL, CELL), egui::Sense::hover());
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(9.0),
            if strong {
                ui.visuals().text_color()
            } else {
                ui.visuals().weak_text_color()
            },
        );
        resp
    };
    let head_tip =
        |labels: &std::collections::BTreeMap<u32, String>, kind: &str, n: u32| match labels.get(&n)
        {
            Some(name) => format!("{kind} {n}: {name}"),
            None => format!("{kind} {n}"),
        };

    let avail_w = ui.available_width();
    egui::Resize::default()
        .id_salt(("mresize", &path))
        .default_size([avail_w.min(720.0), 360.0])
        .min_height(80.0)
        .show(ui, |ui| {
            egui::ScrollArea::both()
                .id_salt(("mscroll", &path))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(1.0, 1.0);
                    // Resizable row-label column width (persisted by egui/eframe).
                    let lw_id = egui::Id::new(("matrix_label_w", &path));
                    let mut label_w = ui
                        .ctx()
                        .data_mut(|d| d.get_persisted::<f32>(lw_id))
                        .unwrap_or(96.0);

                    // Draw a left row-label cell (signal number + name), clipped to width.
                    let row_label = |ui: &mut egui::Ui, w: f32, num: u32, name: Option<&String>| {
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(w, CELL), egui::Sense::click());
                        let text = match name {
                            Some(n) => format!("{num} {n}"),
                            None => num.to_string(),
                        };
                        // painter_at clips the text to the cell, so long names truncate.
                        ui.painter_at(rect).text(
                            egui::pos2(rect.left() + 2.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            text,
                            egui::FontId::proportional(10.0),
                            ui.visuals().text_color(),
                        );
                        resp
                    };

                    // Resizable column-header height. Default tall enough to show rotated
                    // names when the columns have labels (else compact). Only the user's
                    // drag is persisted — otherwise the height-18 captured before the
                    // labels arrive would stick and the names would never rotate in.
                    let hh_id = egui::Id::new(("matrix_header_h", &path));
                    let default_hh = if col_labels.is_empty() { 18.0 } else { 76.0 };
                    let mut header_h = ui
                        .ctx()
                        .data_mut(|d| d.get_persisted::<f32>(hh_id))
                        .unwrap_or(default_hh);

                    ui.horizontal(|ui| {
                        // Corner doubles as a 2D resize handle: x → label width, y → header height.
                        let (crect, cresp) = ui.allocate_exact_size(
                            egui::vec2(label_w, header_h),
                            egui::Sense::drag(),
                        );
                        ui.painter().text(
                            crect.right_bottom() - egui::vec2(2.0, 1.0),
                            egui::Align2::RIGHT_BOTTOM,
                            format!("{row_kind}\\{col_kind}"),
                            egui::FontId::proportional(9.0),
                            ui.visuals().weak_text_color(),
                        );
                        if cresp.dragged() {
                            label_w = (label_w + cresp.drag_delta().x).clamp(24.0, 480.0);
                            header_h = (header_h + cresp.drag_delta().y).clamp(18.0, 240.0);
                            ui.ctx().data_mut(|d| {
                                d.insert_persisted(lw_id, label_w);
                                d.insert_persisted(hh_id, header_h);
                            });
                        }
                        cresp
                            .on_hover_cursor(egui::CursorIcon::ResizeNwSe)
                            .on_hover_text("drag to resize the label column / header height");
                        for &c in col_signals {
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(CELL, header_h),
                                egui::Sense::click(),
                            );
                            let name = col_labels.get(&c).filter(|_| header_h > 24.0);
                            if let Some(n) = name {
                                // Rotated column name reading bottom-to-top.
                                let text = format!("{c} {n}");
                                let galley = ui.painter().layout_no_wrap(
                                    text,
                                    egui::FontId::proportional(10.0),
                                    ui.visuals().text_color(),
                                );
                                let mut shape = egui::epaint::TextShape::new(
                                    egui::pos2(
                                        rect.center().x - galley.size().y / 2.0,
                                        rect.bottom() - 2.0,
                                    ),
                                    galley,
                                    ui.visuals().text_color(),
                                );
                                shape.angle = -std::f32::consts::FRAC_PI_2;
                                ui.painter_at(rect).add(shape);
                            } else {
                                ui.painter_at(rect).text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    c.to_string(),
                                    egui::FontId::proportional(9.0),
                                    ui.visuals().weak_text_color(),
                                );
                            }
                            let resp = resp
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text(format!(
                                    "{}\n(click for signal parameters)",
                                    head_tip(col_labels, col_kind, c)
                                ));
                            if resp.clicked() {
                                header_clicked = Some((targets_on_top, c));
                            }
                        }
                    });
                    for (ri, &r) in row_signals.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let rl = row_label(ui, label_w, r, row_labels.get(&r))
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text(format!(
                                    "{}\n(click for signal parameters)",
                                    head_tip(row_labels, row_kind, r)
                                ));
                            if rl.clicked() {
                                header_clicked = Some((!targets_on_top, r));
                            }
                            let row_bg = if ri % 2 == 0 { light_row } else { dark_row };
                            for &c in col_signals {
                                let (t, s) = if targets_on_top { (c, r) } else { (r, c) };
                                let on = m.connections.get(&t).is_some_and(|set| set.contains(&s));
                                let (rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(CELL, CELL),
                                    egui::Sense::click(),
                                );
                                let fill = if on {
                                    crosspoint
                                } else if resp.hovered() {
                                    ui.visuals().widgets.hovered.bg_fill
                                } else {
                                    row_bg
                                };
                                ui.painter().rect_filled(rect, 2.0, fill);
                                let tname = m
                                    .target_labels
                                    .get(&t)
                                    .map(|n| format!(" ({n})"))
                                    .unwrap_or_default();
                                let sname = m
                                    .source_labels
                                    .get(&s)
                                    .map(|n| format!(" ({n})"))
                                    .unwrap_or_default();
                                let resp = resp.on_hover_text(format!(
                                    "target {t}{tname} <- source {s}{sname}"
                                ));
                                if resp.clicked() {
                                    let operation = if on {
                                        glow::connection_operation::DISCONNECT
                                    } else if m.mtype == glow::matrix_type::N_TO_N {
                                        glow::connection_operation::CONNECT
                                    } else {
                                        glow::connection_operation::ABSOLUTE
                                    };
                                    commands.push(NetCommand::MatrixConnect {
                                        path: path.clone(),
                                        target: t,
                                        sources: vec![s],
                                        operation,
                                    });
                                }
                            }
                        });
                    }
                });
        });
    header_clicked
}
