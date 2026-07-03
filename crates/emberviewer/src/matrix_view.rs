//! The matrix crosspoint grid - shared by the desktop app and the wasm browser
//! client. Targets-on-top puts targets across the columns and sources down the
//! rows (swap with `targets_on_top = false`). Returns `Some((is_target, signal))`
//! when a row/column header is clicked, so the caller can show that signal's
//! parameters.

use ember_proto::glow;

use crate::model::{Entry, MatrixInfo};
use crate::net::NetCommand;

/// Renders the matrix. Returns `(header_clicked, blocked)`: `header_clicked`
/// is `Some((is_target, signal))` when a row/column header was clicked, and
/// `blocked` is true when a crosspoint was clicked while not armed (so the
/// caller can flash the padlock). The lock deliberately gates only routing -
/// resizing, scrolling, and header clicks stay live while locked.
pub fn render_matrix(
    ui: &mut egui::Ui,
    entry: &Entry,
    m: &MatrixInfo,
    targets_on_top: bool,
    armed: bool,
    commands: &mut Vec<NetCommand>,
) -> (Option<(bool, u32)>, bool) {
    let mut header_clicked: Option<(bool, u32)> = None;
    let mut blocked = false;
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

    let avail_w = ui.available_width();
    // The grid scrolls freely inside the Resize; the header row and label column
    // are repainted afterwards, pinned to the viewport edges (see the sticky
    // overlay below, which paints/interacts without allocating layout space so
    // it cannot feed back into the Resize/ScrollArea sizing).
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

                    // Virtualized grid: allocate one rect spanning the whole grid so
                    // the scrollbars reflect the full extent, then paint and hit-test
                    // only the cells inside the viewport. On a 5001x5001 matrix this
                    // is a few hundred cells per frame instead of 25 million.
                    let n_cols = col_signals.len();
                    let n_rows = row_signals.len();
                    let col0 = origin.x + label_w + 1.0; // left of column 0
                    let row0 = origin.y + header_h + 1.0; // top of row 0
                    let step = CELL + 1.0;
                    let total_w = label_w + n_cols as f32 * step;
                    let total_h = header_h + n_rows as f32 * step;
                    let (_, grid_resp) =
                        ui.allocate_exact_size(egui::vec2(total_w, total_h), egui::Sense::click());

                    let (first_col, last_col) =
                        visible_range(col0, step, n_cols, view.left(), view.right());
                    let (first_row, last_row) =
                        visible_range(row0, step, n_rows, view.top(), view.bottom());

                    let painter = ui.painter().clone();
                    // Indexing (not iterators) is deliberate: we walk only the
                    // visible sub-range and need the absolute index for geometry.
                    #[allow(clippy::needless_range_loop)]
                    for ri in first_row..last_row {
                        let r = row_signals[ri];
                        let top = row0 + ri as f32 * step;
                        let row_bg = if ri % 2 == 0 { light_row } else { dark_row };
                        let in_row = hovered_row == Some(ri);
                        for ci in first_col..last_col {
                            let c = col_signals[ci];
                            let left = col0 + ci as f32 * step;
                            let (t, s) = if targets_on_top { (c, r) } else { (r, c) };
                            let on = m.connections.get(&t).is_some_and(|set| set.contains(&s));
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
                            painter.rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(left, top),
                                    egui::vec2(CELL, CELL),
                                ),
                                2.0,
                                fill,
                            );
                        }
                    }

                    // One hit-test for the hovered cell. `hovered_col`/`hovered_row`
                    // already return None over the sticky header/label strips (the
                    // pointer is gated on label_x/header_y), so cells scrolled under
                    // the strips don't react - the suppression now tracks the exact
                    // strip edges rather than a cell-center test.
                    if let (Some(ci), Some(ri)) = (hovered_col, hovered_row) {
                        let (c, r) = (col_signals[ci], row_signals[ri]);
                        let (t, s) = if targets_on_top { (c, r) } else { (r, c) };
                        let on = m.connections.get(&t).is_some_and(|set| set.contains(&s));
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
                        let grid_resp = grid_resp
                            .on_hover_text(format!("target {t}{tname} <- source {s}{sname}"));
                        let grid_resp = if armed {
                            grid_resp
                        } else {
                            grid_resp.on_hover_cursor(egui::CursorIcon::NotAllowed)
                        };
                        if grid_resp.clicked() {
                            if !armed {
                                // Locked: don't route; the caller flashes the padlock.
                                blocked = true;
                            } else {
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
                    }
                    (origin, view, hovered_col, hovered_row)
                })
                .inner
        });

    // Sticky header/label overlay: repaint the header row pinned to the top and
    // the label column pinned to the left, carrying their clicks and the resize
    // handles. This paints into the PARENT panel's layer, after the ScrollArea:
    // later paint order puts it above the scrolling grid, while egui Windows
    // (Order::Middle, e.g. About/Options) stay above the whole panel - a
    // separate Foreground Area used to draw over those windows. `ui.interact`
    // registers widgets without allocating layout space, so none of this feeds
    // back into the Resize/ScrollArea sizing.
    let label_x = view.left() + label_w;
    let header_y = view.top() + header_h;
    let col_left = |ci: usize| origin.x + label_w + 1.0 + ci as f32 * (CELL + 1.0);
    let row_top = |ri: usize| origin.y + header_h + 1.0 + ri as f32 * (CELL + 1.0);
    // First line only: some sources carry no name, so the identifier is a
    // multi-line SDP blob that would otherwise spill down across other rows.
    let short = |s: &str| s.split(['\n', '\r']).next().unwrap_or(s).to_owned();
    // Base for the overlay's stable widget IDs (order-independent; see below).
    let oid = egui::Id::new(("msticky", &path));

    {
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
            let rect =
                egui::Rect::from_min_size(egui::pos2(left, view.top()), egui::vec2(CELL, header_h));
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
            // Explicit stable ID (not allocate_rect's order-derived one):
            // the set of visible headers changes while scrolling/resizing,
            // and shifting auto-IDs would break clicks and drag locks.
            let resp = ui
                .interact(rect, oid.with(("mhdr", c)), egui::Sense::click())
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
            let rect =
                egui::Rect::from_min_size(egui::pos2(view.left(), top), egui::vec2(label_w, CELL));
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
                .interact(rect, oid.with(("mrow", r)), egui::Sense::click())
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
        // Stable IDs are essential here: dragging moves label_x/header_y,
        // which changes how many header/label widgets get skipped above -
        // with order-derived auto-IDs the splitter's ID shifted mid-drag
        // and egui released the drag lock ("lets go" while resizing).
        let vresp = ui
            .interact(
                egui::Rect::from_min_max(
                    egui::pos2(label_x - grab, view.top()),
                    egui::pos2(label_x + grab, view.bottom()),
                ),
                oid.with("msplit_v"),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
            .on_hover_text("drag to resize the label column width");
        if vresp.dragged() {
            let new_w = (label_w + vresp.drag_delta().x).clamp(24.0, 480.0);
            ui.ctx().data_mut(|d| d.insert_persisted(lw_id, new_w));
        }
        let hresp = ui
            .interact(
                egui::Rect::from_min_max(
                    egui::pos2(view.left(), header_y - grab),
                    egui::pos2(view.right(), header_y + grab),
                ),
                oid.with("msplit_h"),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeVertical)
            .on_hover_text("drag to resize the header row height");
        if hresp.dragged() {
            let new_h = (header_h + hresp.drag_delta().y).clamp(18.0, 240.0);
            ui.ctx().data_mut(|d| d.insert_persisted(hh_id, new_h));
        }
    }
    (header_clicked, blocked)
}

/// Half-open index range `[first, last)` of a uniform axis that intersects the
/// visible window, for grid virtualization. `n` items sit `step` apart with the
/// first item's leading edge at `content_start` (screen coords); `view_min` /
/// `view_max` are the viewport edges. Result is clamped to `0..=n`. `first` uses
/// a floor so the item straddling `view_min` is included; `last` rounds up so the
/// item straddling `view_max` is included.
fn visible_range(
    content_start: f32,
    step: f32,
    n: usize,
    view_min: f32,
    view_max: f32,
) -> (usize, usize) {
    if n == 0 || step <= 0.0 {
        return (0, 0);
    }
    let rel_max = view_max - content_start;
    if rel_max < 0.0 {
        return (0, 0);
    }
    let rel_min = view_min - content_start;
    let first = if rel_min <= 0.0 {
        0
    } else {
        ((rel_min / step) as usize).min(n)
    };
    let last = ((rel_max / step) as usize + 1).min(n);
    (first, last.max(first))
}

#[cfg(test)]
mod tests {
    use super::visible_range;

    // A uniform axis: 100 items, 19px apart (CELL 18 + 1 spacing), first at x=50.
    const START: f32 = 50.0;
    const STEP: f32 = 19.0;
    const N: usize = 100;

    #[test]
    fn full_axis_visible_when_window_covers_all() {
        assert_eq!(visible_range(START, STEP, N, 0.0, 10_000.0), (0, N));
    }

    #[test]
    fn window_before_content_is_empty() {
        assert_eq!(visible_range(START, STEP, N, 0.0, 40.0), (0, 0));
    }

    #[test]
    fn window_past_content_clamps_to_n() {
        let (first, last) = visible_range(START, STEP, N, 9_000.0, 10_000.0);
        assert_eq!(first, N);
        assert_eq!(last, N);
    }

    #[test]
    fn includes_items_straddling_both_edges() {
        // Window [100, 200] over items starting at 50, step 19.
        // item i occupies [50 + 19i, 50 + 19i + 18].
        // left edge <= 100 -> i <= 2 (item 2 left = 88); floor((100-50)/19)=2.
        // right edge: floor((200-50)/19)=7 -> last = 8 (item 7 left = 183 <= 200).
        let (first, last) = visible_range(START, STEP, N, 100.0, 200.0);
        assert_eq!(first, 2);
        assert_eq!(last, 8);
        // Every painted item must actually overlap the window.
        for i in first..last {
            let left = START + i as f32 * STEP;
            let right = left + 18.0;
            assert!(right >= 100.0 && left <= 200.0, "item {i} out of window");
        }
    }

    #[test]
    fn zero_items_is_empty() {
        assert_eq!(visible_range(START, STEP, 0, 0.0, 1000.0), (0, 0));
    }
}
