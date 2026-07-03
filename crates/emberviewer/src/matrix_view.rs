//! The matrix crosspoint grid - shared by the desktop app and the wasm browser
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
    // Crosshair highlight following the hovered row/column, carried all the way
    // into the labels so it is obvious which crosspoint is under the pointer.
    // `cross` tints the band, `cross_strong` the exact row/column intersection.
    let (cross, cross_strong) = if dark {
        (
            egui::Color32::from_rgb(52, 74, 110),
            egui::Color32::from_rgb(74, 108, 158),
        )
    } else {
        (
            egui::Color32::from_rgb(200, 218, 242),
            egui::Color32::from_rgb(158, 190, 230),
        )
    };
    // Opaque backing for the sticky header/label overlay so the grid scrolling
    // underneath does not show through.
    let overlay_bg = ui.visuals().panel_fill;

    const CELL: f32 = 18.0;
    let head_tip =
        |labels: &std::collections::BTreeMap<u32, String>, kind: &str, n: u32| match labels.get(&n)
        {
            Some(name) => format!("{kind} {n}: {name}"),
            None => format!("{kind} {n}"),
        };

    // Row-label column width and column-header height are user-resizable via the
    // top-left corner and persisted by egui/eframe.
    let lw_id = egui::Id::new(("matrix_label_w", &path));
    let hh_id = egui::Id::new(("matrix_header_h", &path));
    let label_w = ui
        .ctx()
        .data_mut(|d| d.get_persisted::<f32>(lw_id))
        .unwrap_or(96.0);
    // Column-header height defaults tall enough to show rotated names when the
    // columns have labels (else compact). Only the user's drag is persisted -
    // otherwise the height-18 captured before the labels arrive would stick and
    // the names would never rotate in.
    let default_hh = if col_labels.is_empty() { 18.0 } else { 76.0 };
    let header_h = ui
        .ctx()
        .data_mut(|d| d.get_persisted::<f32>(hh_id))
        .unwrap_or(default_hh);

    let enabled = ui.is_enabled();
    let avail_w = ui.available_width();
    // The grid scrolls freely inside the Resize; the header row and label column
    // are repainted afterwards by a floating overlay Area pinned to the viewport
    // edges. The overlay is deliberately NOT a child of this ui, so its widgets
    // cannot feed back into the Resize/ScrollArea sizing (which otherwise made the
    // matrix window grow without bound and swallowed the horizontal scrollbar).
    let (origin, view, hovered_col, hovered_row) = egui::Resize::default()
        .id_salt(("mresize", &path))
        .default_size([avail_w.min(720.0), 360.0])
        .min_height(80.0)
        .show(ui, |ui| {
            egui::ScrollArea::both()
                .id_salt(("mscroll", &path))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Content origin (scrolls) and the fixed viewport, in screen
                    // space. Everything below derives cell geometry from these so
                    // the crosshair and the sticky overlay agree pixel-for-pixel.
                    let origin = ui.cursor().min;
                    let view = ui.clip_rect();
                    ui.spacing_mut().item_spacing = egui::vec2(1.0, 1.0);

                    // Screen edges of the sticky header row / label column.
                    let label_x = view.left() + label_w;
                    let header_y = view.top() + header_h;

                    // Which column/row is the pointer over? Derived from geometry
                    // (not per-cell hover) so the crosshair lights up over headers
                    // and labels too, and stays correct beneath the sticky overlay.
                    let ptr = ui.ctx().pointer_hover_pos().filter(|p| view.contains(*p));
                    let hovered_col = ptr.and_then(|p| {
                        if p.x < label_x {
                            return None;
                        }
                        let rel = p.x - (origin.x + label_w + 1.0);
                        let step = (rel / (CELL + 1.0)).floor();
                        let idx = step as usize;
                        (rel >= 0.0 && idx < col_signals.len() && rel - step * (CELL + 1.0) <= CELL)
                            .then_some(idx)
                    });
                    let hovered_row = ptr.and_then(|p| {
                        if p.y < header_y {
                            return None;
                        }
                        let rel = p.y - (origin.y + header_h + 1.0);
                        let step = (rel / (CELL + 1.0)).floor();
                        let idx = step as usize;
                        (rel >= 0.0 && idx < row_signals.len() && rel - step * (CELL + 1.0) <= CELL)
                            .then_some(idx)
                    });

                    // Reserve the header row (corner + column headers). The real,
                    // interactive header is painted by the sticky overlay below;
                    // here we only claim the space so the grid scrolls under it.
                    ui.horizontal(|ui| {
                        ui.allocate_exact_size(egui::vec2(label_w, header_h), egui::Sense::hover());
                        for _ in col_signals {
                            ui.allocate_exact_size(
                                egui::vec2(CELL, header_h),
                                egui::Sense::hover(),
                            );
                        }
                    });
                    for (ri, &r) in row_signals.iter().enumerate() {
                        ui.horizontal(|ui| {
                            // Reserve the sticky row-label cell (painted by overlay).
                            ui.allocate_exact_size(egui::vec2(label_w, CELL), egui::Sense::hover());
                            let row_bg = if ri % 2 == 0 { light_row } else { dark_row };
                            let in_row = hovered_row == Some(ri);
                            for (ci, &c) in col_signals.iter().enumerate() {
                                let (t, s) = if targets_on_top { (c, r) } else { (r, c) };
                                let on = m.connections.get(&t).is_some_and(|set| set.contains(&s));
                                let (rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(CELL, CELL),
                                    egui::Sense::click(),
                                );
                                let in_col = hovered_col == Some(ci);
                                let fill = if on {
                                    crosspoint
                                } else if in_row && in_col {
                                    cross_strong
                                } else if in_row || in_col {
                                    cross
                                } else {
                                    row_bg
                                };
                                ui.painter().rect_filled(rect, 2.0, fill);
                                // Cells scrolled under the sticky header/label are
                                // hidden by the overlay - don't let them react.
                                if rect.center().x < label_x || rect.center().y < header_y {
                                    continue;
                                }
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
                    (origin, view, hovered_col, hovered_row)
                })
                .inner
        });

    // Sticky header/label overlay. A floating foreground Area repaints the header
    // row pinned to the top and the label column pinned to the left, carrying
    // their clicks and the resize handles. Being a separate Area (not a child ui)
    // it floats over the grid without contributing to any layout.
    let label_x = view.left() + label_w;
    let header_y = view.top() + header_h;
    let col_left = |ci: usize| origin.x + label_w + 1.0 + ci as f32 * (CELL + 1.0);
    let row_top = |ri: usize| origin.y + header_h + 1.0 + ri as f32 * (CELL + 1.0);
    // First line only: some sources carry no name, so the identifier is a
    // multi-line SDP blob that would otherwise spill down across other rows.
    let short = |s: &str| s.split(['\n', '\r']).next().unwrap_or(s).to_owned();

    egui::Area::new(egui::Id::new(("msticky", &path)))
        .order(egui::Order::Foreground)
        .fixed_pos(view.min)
        .constrain(false)
        .show(ui.ctx(), |ui| {
            ui.set_clip_rect(view);
            // Locked (greyed) grid -> keep the overlay inert and greyed to match.
            if !enabled {
                ui.disable();
            }

            // The frozen L-shape: header strip along the top, label strip down the
            // left, corner where they meet. Paint opaque backing first.
            let header_strip = egui::Rect::from_min_max(
                egui::pos2(label_x, view.top()),
                egui::pos2(view.right(), header_y),
            );
            let label_strip = egui::Rect::from_min_max(
                egui::pos2(view.left(), header_y),
                egui::pos2(label_x, view.bottom()),
            );
            let corner_rect = egui::Rect::from_min_max(view.min, egui::pos2(label_x, header_y));
            let painter = ui.painter().clone();
            painter.rect_filled(header_strip, 0.0, overlay_bg);
            painter.rect_filled(label_strip, 0.0, overlay_bg);
            painter.rect_filled(corner_rect, 0.0, overlay_bg);
            let head_painter = painter.with_clip_rect(header_strip);
            let label_painter = painter.with_clip_rect(label_strip);

            // Column headers (skip any scrolled behind the corner/label).
            for (ci, &c) in col_signals.iter().enumerate() {
                let left = col_left(ci);
                if left + CELL <= label_x || left >= view.right() {
                    continue;
                }
                let rect = egui::Rect::from_min_size(
                    egui::pos2(left, view.top()),
                    egui::vec2(CELL, header_h),
                );
                if hovered_col == Some(ci) {
                    head_painter.rect_filled(rect, 0.0, cross_strong);
                }
                let name = col_labels.get(&c).filter(|_| header_h > 24.0);
                if let Some(n) = name {
                    // Rotated column name reading bottom-to-top.
                    let galley = head_painter.layout_no_wrap(
                        format!("{c} {}", short(n)),
                        egui::FontId::proportional(10.0),
                        ui.visuals().text_color(),
                    );
                    let mut shape = egui::epaint::TextShape::new(
                        egui::pos2(rect.center().x - galley.size().y / 2.0, rect.bottom() - 2.0),
                        galley,
                        ui.visuals().text_color(),
                    );
                    shape.angle = -std::f32::consts::FRAC_PI_2;
                    head_painter.add(shape);
                } else {
                    head_painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        c.to_string(),
                        egui::FontId::proportional(9.0),
                        ui.visuals().weak_text_color(),
                    );
                }
                let resp = ui
                    .allocate_rect(rect, egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(format!(
                        "{}\n(click for signal parameters)",
                        head_tip(col_labels, col_kind, c)
                    ));
                if resp.clicked() {
                    header_clicked = Some((targets_on_top, c));
                }
            }

            // Row labels (skip any scrolled behind the corner/header).
            for (ri, &r) in row_signals.iter().enumerate() {
                let top = row_top(ri);
                if top + CELL <= header_y || top >= view.bottom() {
                    continue;
                }
                let rect = egui::Rect::from_min_size(
                    egui::pos2(view.left(), top),
                    egui::vec2(label_w, CELL),
                );
                if hovered_row == Some(ri) {
                    label_painter.rect_filled(rect, 0.0, cross_strong);
                }
                let text = match row_labels.get(&r) {
                    Some(n) => format!("{r} {}", short(n)),
                    None => r.to_string(),
                };
                label_painter.text(
                    egui::pos2(rect.left() + 2.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    text,
                    egui::FontId::proportional(10.0),
                    ui.visuals().text_color(),
                );
                let resp = ui
                    .allocate_rect(rect, egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(format!(
                        "{}\n(click for signal parameters)",
                        head_tip(row_labels, row_kind, r)
                    ));
                if resp.clicked() {
                    header_clicked = Some((!targets_on_top, r));
                }
            }

            // Corner label.
            painter.with_clip_rect(corner_rect).text(
                corner_rect.right_bottom() - egui::vec2(2.0, 1.0),
                egui::Align2::RIGHT_BOTTOM,
                format!("{row_kind}\\{col_kind}"),
                egui::FontId::proportional(9.0),
                ui.visuals().weak_text_color(),
            );

            // Resize splitters. The whole right border of the label column drags
            // its width; the whole bottom border of the header row drags its
            // height - like a table divider. Drawn last so they own the border.
            let divider = egui::Color32::from_gray(if dark { 90 } else { 165 });
            let grab = 3.0;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(label_x - 0.5, view.top()),
                    egui::pos2(label_x + 0.5, view.bottom()),
                ),
                0.0,
                divider,
            );
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(view.left(), header_y - 0.5),
                    egui::pos2(view.right(), header_y + 0.5),
                ),
                0.0,
                divider,
            );
            let vresp = ui
                .allocate_rect(
                    egui::Rect::from_min_max(
                        egui::pos2(label_x - grab, view.top()),
                        egui::pos2(label_x + grab, view.bottom()),
                    ),
                    egui::Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                .on_hover_text("drag to resize the label column width");
            if vresp.dragged() {
                let new_w = (label_w + vresp.drag_delta().x).clamp(24.0, 480.0);
                ui.ctx().data_mut(|d| d.insert_persisted(lw_id, new_w));
            }
            let hresp = ui
                .allocate_rect(
                    egui::Rect::from_min_max(
                        egui::pos2(view.left(), header_y - grab),
                        egui::pos2(view.right(), header_y + grab),
                    ),
                    egui::Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeVertical)
                .on_hover_text("drag to resize the header row height");
            if hresp.dragged() {
                let new_h = (header_h + hresp.drag_delta().y).clamp(18.0, 240.0);
                ui.ctx().data_mut(|d| d.insert_persisted(hh_id, new_h));
            }
        });
    header_clicked
}
