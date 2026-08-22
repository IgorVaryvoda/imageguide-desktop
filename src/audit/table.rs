use super::*;

/// Rows are for scanning a folder of thousands, so they are sized to fit as many
/// as possible while still showing a thumbnail you can recognise.
const ROW_HEIGHT: f32 = 40.;
const THUMB_SLOT: f32 = 34.;

// ── Column widths ───────────────────────────────────────────────────────────
// One constant per column, shared by the header and every row. They used to be
// written twice and had already drifted; a header that sits over the wrong column
// is worse than no header.
const W_TICK: f32 = 34.;
const W_FORMAT: f32 = 82.;
const W_PIXELS: f32 = 96.;
const W_DENSITY: f32 = 74.;
const W_WEIGHT: f32 = 100.;
const W_RESULT: f32 = 112.;
/// The Sirv diff column. Wide windows only: below 900px the name needs the
/// room more than the status does.
const W_SYNC: f32 = 86.;
const W_FORMAT_COMPACT: f32 = 70.;
const W_PIXELS_COMPACT: f32 = 88.;
const W_DENSITY_COMPACT: f32 = 60.;
const W_WEIGHT_COMPACT: f32 = 86.;
pub(super) const W_NAME_MIN: f32 = 140.;

/// The audit list, as the component library's virtualised table.
///
/// It was a `uniform_list` with the header, the column widths, the sort arrows and
/// the hit testing all written by hand and kept in step by hand. The delegate hands
/// all of that to the library, which is also where column resizing and dragging come
/// from for free.
pub(super) struct AuditTable {
    /// Weak, because the audit owns the table state, which owns this.
    audit: gpui::WeakEntity<Audit>,
    columns: Vec<TableColumn>,
    /// Width for the name column, recomputed from the window so the fixed columns
    /// do not leave an empty strip on the right. Columns here take a width, not a
    /// share, so somebody has to do the arithmetic.
    name_width: f32,
    compact: bool,
    show_result: bool,
}

/// The columns, in display order. `Column` is what the audit sorts by; this adds the
/// ones that carry no sortable value of their own.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TableColumn {
    Tick,
    Thumb,
    Name,
    Format,
    Pixels,
    Density,
    Weight,
    Sync,
    Result,
}

impl TableColumn {
    /// The audit column this sorts by, if it sorts.
    fn sorts_by(&self) -> Option<Column> {
        match self {
            TableColumn::Name => Some(Column::Name),
            TableColumn::Format => Some(Column::Format),
            TableColumn::Pixels => Some(Column::Pixels),
            TableColumn::Density => Some(Column::Density),
            TableColumn::Weight => Some(Column::Weight),
            _ => None,
        }
    }

    fn spec(&self, name_width: f32, compact: bool) -> TableCol {
        match self {
            TableColumn::Tick => TableCol::new("tick", "").width(px(W_TICK)),
            TableColumn::Thumb => TableCol::new("thumb", "").width(px(THUMB_SLOT + 12.)).p_0(),
            // Name takes whatever the other columns leave, so the window has no dead
            // strip down its right-hand side.
            TableColumn::Name => TableCol::new("name", "Name")
                .width(px(name_width))
                .min_width(px(W_NAME_MIN))
                .sortable()
                .resizable(true),
            TableColumn::Format => TableCol::new("format", "Format")
                .width(px(if compact { W_FORMAT_COMPACT } else { W_FORMAT }))
                .sortable(),
            TableColumn::Pixels => TableCol::new("pixels", "Size")
                .width(px(if compact { W_PIXELS_COMPACT } else { W_PIXELS }))
                .text_right()
                .sortable(),
            TableColumn::Density => TableCol::new("density", "B/px")
                .width(px(if compact {
                    W_DENSITY_COMPACT
                } else {
                    W_DENSITY
                }))
                .text_right()
                .sortable(),
            TableColumn::Weight => TableCol::new("weight", "Weight")
                .width(px(if compact { W_WEIGHT_COMPACT } else { W_WEIGHT }))
                .text_right()
                .sortable(),
            TableColumn::Sync => TableCol::new("sirv", "Sirv").width(px(W_SYNC)),
            TableColumn::Result => TableCol::new("result", "Result")
                .width(px(W_RESULT))
                .text_right(),
        }
    }
}

impl AuditTable {
    /// Chrome the table spends on gaps, cell padding and its own border.
    const CHROME: f32 = 30.;

    fn fixed_width(compact: bool, show_result: bool) -> f32 {
        W_TICK
            + THUMB_SLOT
            + 12.
            + if compact { W_FORMAT_COMPACT } else { W_FORMAT }
            + if compact { W_PIXELS_COMPACT } else { W_PIXELS }
            + if compact {
                W_DENSITY_COMPACT
            } else {
                W_DENSITY
            }
            + if compact { W_WEIGHT_COMPACT } else { W_WEIGHT }
            + if compact { 0. } else { W_SYNC }
            + if show_result { W_RESULT } else { 0. }
    }

    pub(super) fn layout(width: f32, show_result: bool) -> (bool, f32, Vec<TableColumn>) {
        let compact = width < 900.;
        let name_width =
            (width - Self::fixed_width(compact, show_result) - Self::CHROME).max(W_NAME_MIN);
        let columns = if compact {
            vec![
                TableColumn::Tick,
                TableColumn::Thumb,
                TableColumn::Name,
                TableColumn::Format,
                TableColumn::Pixels,
                TableColumn::Density,
                TableColumn::Weight,
                TableColumn::Result,
            ]
        } else {
            vec![
                TableColumn::Tick,
                TableColumn::Thumb,
                TableColumn::Name,
                TableColumn::Format,
                TableColumn::Pixels,
                TableColumn::Density,
                TableColumn::Weight,
                TableColumn::Sync,
                TableColumn::Result,
            ]
        };
        (compact, name_width, columns)
    }

    pub(super) fn new(audit: gpui::WeakEntity<Audit>, window: &Window) -> Self {
        let mut table = Self {
            audit,
            name_width: W_NAME_MIN,
            compact: false,
            show_result: false,
            columns: Vec::new(),
        };
        table.set_viewport_width(f32::from(window.viewport_size().width), false);
        table
    }

    pub(super) fn set_viewport_width(&mut self, width: f32, show_result: bool) {
        self.show_result = show_result;
        (self.compact, self.name_width, self.columns) = Self::layout(width, show_result);
    }
}

impl TableDelegate for AuditTable {
    fn columns_count(&self, _cx: &App) -> usize {
        // The result column only exists once there is something to put in it.
        // Reserving its width up front left a fifth of the window empty in the
        // common case.
        if self.show_result {
            self.columns.len()
        } else {
            self.columns.len() - 1
        }
    }

    fn rows_count(&self, cx: &App) -> usize {
        self.audit
            .upgrade()
            .map_or(0, |audit| audit.read(cx).visible.len())
    }

    fn column(&self, col_ix: usize, cx: &App) -> TableCol {
        let Some(column) = self.columns.get(col_ix) else {
            return TableCol::new("none", "");
        };
        let mut spec = column.spec(self.name_width, self.compact);
        // Show the arrow on whichever column the audit is actually ordered by.
        if let Some(sort) = column.sorts_by()
            && let Some(audit) = self.audit.upgrade()
        {
            let audit = audit.read(cx);
            if audit.sort.column == sort {
                spec = if audit.sort.descending {
                    spec.descending()
                } else {
                    spec.ascending()
                };
            }
        }
        spec
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        _sort: ColumnSort,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(column) = self.columns.get(col_ix).and_then(TableColumn::sorts_by) else {
            return;
        };
        let Some(audit) = self.audit.upgrade() else {
            return;
        };
        audit.update(cx, |audit, cx| audit.set_sort(column, cx));
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui::Stateful<gpui::Div> {
        let row = div().id(("row", row_ix));
        let Some(audit) = self.audit.upgrade() else {
            return row;
        };
        let audit_state = audit.read(cx);
        let ticked = audit_state
            .entry_at(row_ix)
            .is_some_and(|entry| audit_state.selected.contains(&entry));
        let cursor = audit_state.cursor;

        // The audit's finding, carried on the row's left edge: a tick in the
        // density band colour, so the shape of the folder is visible while
        // scrolling and not only in the B/px column.
        let rail = audit_state
            .entry_at(row_ix)
            .and_then(|index| audit_state.entries.get(index))
            .map(|entry| density_colour(entry.bytes_per_pixel(), cx));
        row.h(px(ROW_HEIGHT))
            .relative()
            .border_1()
            .border_color(gpui::transparent_black())
            .when(ticked, |row| row.bg(cx.theme().list_active))
            .when(row_ix == cursor, |row| row.border_color(cx.theme().ring))
            .children(rail.map(|colour| {
                div()
                    .absolute()
                    .left_0()
                    .top(px(5.))
                    .bottom(px(5.))
                    .w(px(2.))
                    .rounded_full()
                    .bg(colour.opacity(0.9))
            }))
            .on_click(cx.listener(move |table, event: &gpui::ClickEvent, _, cx| {
                let Some(audit) = table.delegate().audit.upgrade() else {
                    return;
                };
                audit.update(cx, |audit, cx| audit.click_row(row_ix, event, cx));
            }))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(column) = self.columns.get(col_ix).copied() else {
            return div().into_any_element();
        };
        let Some(handle) = self.audit.upgrade() else {
            return div().into_any_element();
        };

        // The row the viewport asked for is the row worth decoding.
        handle.update(cx, |audit, cx| {
            if let Some(entry) = audit.entry_at(row_ix) {
                audit.request_thumb(entry, cx);
            }
        });

        let audit = handle.read(cx);
        let Some(index) = audit.entry_at(row_ix) else {
            return div().into_any_element();
        };
        let Some(entry) = audit.entries.get(index) else {
            return div().into_any_element();
        };

        match column {
            TableColumn::Tick => {
                let ticked = audit.selected.contains(&index);
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .debug_selector(move || format!("table-checkbox-{index}"))
                    .on_key_down(cx.listener(|_, event, _, cx| {
                        if is_checkbox_activation_key(event) {
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        Checkbox::new(("tick", index))
                            .checked(ticked)
                            .on_click(cx.listener(move |table, _: &bool, _, cx| {
                                cx.stop_propagation();
                                let Some(audit) = table.delegate().audit.upgrade() else {
                                    return;
                                };
                                audit.update(cx, |audit, cx| {
                                    if audit.converting {
                                        return;
                                    }
                                    if !audit.selected.remove(&index) {
                                        audit.selected.insert(index);
                                    }
                                    audit.schedule_estimate(cx);
                                    cx.notify();
                                });
                            })),
                    )
                    .into_any_element()
            }
            TableColumn::Thumb => div()
                .w(px(THUMB_SLOT))
                .h(px(THUMB_SLOT))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .bg(cx.theme().background)
                // A fixed slot, so rows do not jump as thumbnails arrive.
                .when_some(audit.thumbs.get(&index).cloned(), |slot, image| {
                    slot.child(img(image).max_w(px(THUMB_SLOT)).max_h(px(THUMB_SLOT)))
                })
                .into_any_element(),
            TableColumn::Name => div()
                .w_full()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(cx.theme().foreground)
                .child(entry.name())
                .into_any_element(),
            TableColumn::Format => {
                let lies = entry.extension_lies();
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .whitespace_nowrap()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if lies {
                        cx.theme().yellow
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(format_name(entry.format))
                    // The extension disagrees with the bytes. The mark is small
                    // because the count in the notice above is what raises it.
                    .when(lies, |cell| cell.child(div().text_size(px(11.)).child("≠")))
                    .into_any_element()
            }
            TableColumn::Pixels => div()
                .w_full()
                .flex()
                .justify_end()
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(px(12.))
                .text_color(cx.theme().muted_foreground)
                .whitespace_nowrap()
                .child(format!("{}×{}", entry.width, entry.height))
                .into_any_element(),
            TableColumn::Density => {
                let density = entry.bytes_per_pixel();
                div()
                    .w_full()
                    .flex()
                    .justify_end()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .whitespace_nowrap()
                    .text_color(density_colour(density, cx))
                    .child(format!("{density:.2}"))
                    .into_any_element()
            }
            // The bar lives under its own number now: one cell, so a bar can
            // never drift away from the figure it measures.
            TableColumn::Weight => {
                let fraction = entry.bytes as f32 / audit.heaviest.max(1) as f32;
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_end()
                    .justify_center()
                    .gap_1()
                    .child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .whitespace_nowrap()
                            .text_color(cx.theme().foreground)
                            .child(format_bytes(entry.bytes)),
                    )
                    .child(div().w_full().child(meter(
                        ("weight", index),
                        fraction,
                        cx.theme().primary,
                        3.,
                    )))
                    .into_any_element()
            }
            TableColumn::Sync => {
                // The row's file against the paired Sirv folder. No pairing,
                // no status: the column exists only when it can know.
                let Some((label, colour)) = audit.sync_label(entry, cx) else {
                    return div().into_any_element();
                };
                div()
                    .flex()
                    .items_center()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(11.))
                    .text_color(colour)
                    .child(label)
                    .into_any_element()
            }
            TableColumn::Result => div()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .whitespace_nowrap()
                .when_some(audit.results.get(&index), |slot, converted| {
                    let saved = entry.bytes.saturating_sub(*converted);
                    let percent = if entry.bytes == 0 {
                        0.
                    } else {
                        saved as f32 / entry.bytes as f32 * 100.
                    };
                    // A file that grew is a real outcome, not a rounding error:
                    // re-encoding an already-optimal JPEG usually costs bytes.
                    let grew = *converted > entry.bytes;
                    slot.child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(12.))
                            .text_color(cx.theme().muted_foreground)
                            .child(format_bytes(*converted)),
                    )
                    .child(if grew {
                        Tag::warning().small().child("larger")
                    } else {
                        Tag::success().small().child(format!("−{percent:.0}%"))
                    })
                })
                .into_any_element(),
        }
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_4()
            .w_full()
            .flex()
            .justify_center()
            .text_size(px(12.))
            .text_color(cx.theme().muted_foreground)
            .child("Nothing matches that filter")
    }
}
