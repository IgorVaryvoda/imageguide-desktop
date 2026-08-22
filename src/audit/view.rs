use super::*;

/// A label for the comparison view, which floats over the picture rather than over
/// a theme surface, so it carries its own dark backing.
fn compare_chip(text: impl Into<gpui::SharedString>, colour: gpui::Hsla, _cx: &App) -> gpui::Div {
    div()
        .h(px(18.))
        .px_2()
        .flex()
        .items_center()
        .flex_shrink_0()
        .rounded_md()
        .bg(rgba(0x000000b8))
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(colour)
        .child(text.into())
}

/// A proportional bar. The audit is a ranking and a column of numbers does not
/// rank — 632 KB and 104 KB were set in the same size and colour, so the shape of
/// the folder was invisible in a list sorted by exactly that.
pub(super) fn meter(
    id: impl Into<gpui::ElementId>,
    fraction: f32,
    colour: gpui::Hsla,
    height: f32,
) -> Progress {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0., 1.)
    } else {
        0.
    };
    Progress::new(id)
        .value(fraction * 100.)
        .color(colour)
        .h(px(height))
}

/// One option in a segmented control.
///
fn segment(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    selected: bool,
) -> Button {
    // The group's neutral selected state keeps `primary` for the conversion commit.
    Button::new(id).label(label).selected(selected)
}
impl Audit {
    /// One gallery tile: the picture, with its name and weight under it.
    fn tile(
        &self,
        row: usize,
        index: usize,
        tile_size: f32,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(entry) = self.entries.get(index) else {
            return div().id(("tile", row));
        };
        let thumb = self.thumbs.get(&index).cloned();
        let ticked = self.selected.contains(&index);

        let density = entry.bytes_per_pixel();

        div()
            .id(("tile", row))
            .w(px(tile_size))
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .rounded_lg()
            .bg(cx.theme().secondary)
            // Always bordered, in nothing, so arrowing onto a tile does not shunt
            // its contents a pixel down and right.
            .border_1()
            .border_color(gpui::transparent_black())
            .when(ticked, |tile| {
                tile.bg(cx.theme().list_active)
                    .border_color(cx.theme().list_active_border)
            })
            .when(row == self.cursor, |tile| {
                tile.border_color(cx.theme().ring)
            })
            .hover(|style| style.bg(cx.theme().list_hover))
            .on_click(cx.listener(move |audit, event: &gpui::ClickEvent, _, cx| {
                if let Some(position) = audit.row_of(index) {
                    audit.click_row(position, event, cx);
                }
            }))
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(tile_size - 68.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .overflow_hidden()
                    .bg(cx.theme().background)
                    .when_some(thumb, |slot, image| {
                        slot.child(
                            img(image)
                                .max_w(px(tile_size - 16.))
                                .max_h(px(tile_size - 68.)),
                        )
                    })
                    // The grid had no way to tick anything; the keyboard was the
                    // only route to a selection you could see in the list.
                    .child(
                        div()
                            .absolute()
                            .top(px(4.))
                            .left(px(4.))
                            .debug_selector(move || format!("grid-checkbox-{index}"))
                            .on_key_down(cx.listener(|_, event, _, cx| {
                                if is_checkbox_activation_key(event) {
                                    cx.stop_propagation();
                                }
                            }))
                            .child(
                                Checkbox::new(("tile-tick", index))
                                    .checked(ticked)
                                    .on_click(cx.listener(move |audit, _: &bool, _, cx| {
                                        cx.stop_propagation();
                                        if audit.converting {
                                            return;
                                        }
                                        if !audit.selected.remove(&index) {
                                            audit.selected.insert(index);
                                        }
                                        audit.schedule_estimate(cx);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div().absolute().bottom(px(4.)).right(px(4.)).child(
                            div()
                                .px_1()
                                .rounded_sm()
                                .bg(cx.theme().background.opacity(0.8))
                                .text_size(px(10.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if entry.extension_lies() {
                                    cx.theme().yellow
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(format_name(entry.format))
                                .when(entry.extension_lies(), |label| label.child(" ≠")),
                        ),
                    )
                    // The same word the table's Sirv column uses. The gallery used to
                    // show nothing at all, so switching to grid lost the diff.
                    .children(self.sync_label(entry, cx).map(|(label, colour)| {
                        div().absolute().top(px(4.)).right(px(4.)).child(
                            div()
                                .px_1()
                                .rounded_sm()
                                .bg(cx.theme().background.opacity(0.8))
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(10.))
                                .text_color(colour)
                                .child(label),
                        )
                    })),
            )
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(12.))
                    .text_color(cx.theme().foreground)
                    .child(entry.name()),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(10.))
                    .child(div().text_color(cx.theme().muted_foreground).child(
                        match self.results.get(&index) {
                            Some(bytes) => {
                                format!("{} → {}", format_bytes(entry.bytes), format_bytes(*bytes))
                            }
                            None => format_bytes(entry.bytes),
                        },
                    ))
                    .child(
                        div()
                            .text_color(density_colour(density, cx))
                            .child(format!("{density:.2}")),
                    ),
            )
    }
    /// Several exclusive options as one control, under the word for what they choose.
    /// The old toolbar was thirteen identical ghost buttons in a row with a 12px gap
    /// standing in for grouping, and nothing said which was which.
    fn control_group(
        &self,
        label: &'static str,
        group: ButtonGroup,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // The word sits left of its control, in the same muted voice as the
        // rest of the strip's metadata.
        div()
            .flex()
            .items_center()
            .gap_2()
            .flex_shrink_0()
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_nowrap()
                    .child(label),
            )
            .child(group.small().compact())
    }
    /// Full-window comparison. It opens fitted, because you cannot judge a crop of an
    /// image you have not seen yet, and zooms to 1:1 and beyond — fitting a 5568px
    /// photo into a 900px window hides exactly the artefacts this view exists to show.
    fn compare_view(
        &self,
        comparison: &Comparison,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let viewport = window.viewport_size();
        let (view_w, view_h) = (f32::from(viewport.width), f32::from(viewport.height));
        let entry = self.entries.get(comparison.index);
        let source_bytes = entry.map_or(0, |entry| entry.bytes);
        let name = entry.map(|entry| entry.name()).unwrap_or_default();

        let mut stage = div()
            .id("compare-stage")
            .absolute()
            .inset_0()
            .overflow_hidden()
            .bg(rgb(0x0b0d10))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|audit, event: &gpui::MouseDownEvent, _, cx| {
                    if let Some(comparison) = audit.compare.as_mut() {
                        let at = (f32::from(event.position.x), f32::from(event.position.y));
                        comparison.drag = Some((at, comparison.pan));
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|audit, _: &gpui::MouseUpEvent, _, cx| {
                    if let Some(comparison) = audit.compare.as_mut() {
                        comparison.drag = None;
                        cx.notify();
                    }
                }),
            )
            .on_scroll_wheel(
                cx.listener(move |audit, event: &gpui::ScrollWheelEvent, _, cx| {
                    let Some(comparison) = audit.compare.as_mut() else {
                        return;
                    };
                    let Some(pair) = comparison.pair.as_ref() else {
                        return;
                    };

                    let ticks = match event.delta {
                        gpui::ScrollDelta::Lines(delta) => delta.y,
                        gpui::ScrollDelta::Pixels(delta) => f32::from(delta.y) / 40.,
                    };
                    if ticks == 0. {
                        return;
                    }

                    let fit = (view_w / pair.width as f32)
                        .min(view_h / pair.height as f32)
                        .min(1.);
                    let before = comparison.zoom.unwrap_or(fit);
                    let after = (before * 1.2f32.powf(ticks)).clamp(0.02, 16.);

                    // Keep whatever is under the pointer under the pointer. Without this
                    // zooming walks the image off screen.
                    let pointer = (
                        f32::from(event.position.x) - view_w / 2.,
                        f32::from(event.position.y) - view_h / 2.,
                    );
                    let ratio = after / before;
                    comparison.pan = (
                        pointer.0 - (pointer.0 - comparison.pan.0) * ratio,
                        pointer.1 - (pointer.1 - comparison.pan.1) * ratio,
                    );
                    comparison.zoom = Some(after);
                    cx.notify();
                }),
            )
            .on_mouse_move(
                cx.listener(move |audit, event: &gpui::MouseMoveEvent, _, cx| {
                    let Some(comparison) = audit.compare.as_mut() else {
                        return;
                    };
                    let at = (f32::from(event.position.x), f32::from(event.position.y));

                    match comparison.drag {
                        // Held: pan both sides together, so they stay in register.
                        Some((from, start_pan)) => {
                            comparison.pan =
                                (start_pan.0 + at.0 - from.0, start_pan.1 + at.1 - from.1);
                        }
                        // Free: the divider tracks the pointer.
                        None => comparison.split = (at.0 / view_w).clamp(0., 1.),
                    }
                    cx.notify();
                }),
            );

        // Fit never scales up: a 400px thumbnail blown across a 4K window is just a
        // blurry 400px thumbnail. Computed before the branch because the chrome
        // reports the zoom as well as the image using it.
        let scale = comparison.pair.as_ref().map(|pair| {
            let fit = (view_w / pair.width as f32)
                .min(view_h / pair.height as f32)
                .min(1.);
            comparison.zoom.unwrap_or(fit)
        });

        if let (Some(pair), Some(scale)) = (comparison.pair.as_ref(), scale) {
            let natural = (pair.width as f32, pair.height as f32);
            let (image_w, image_h) = (natural.0 * scale, natural.1 * scale);
            // Negative when the image is larger than the window: that is the crop.
            let left = (view_w - image_w) / 2. + comparison.pan.0;
            let top = (view_h - image_h) / 2. + comparison.pan.1;
            let divider = view_w * comparison.split;

            let placed = |image: &Arc<gpui::RenderImage>| {
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(image_w))
                    .h(px(image_h))
                    .child(img(image.clone()).w(px(image_w)).h(px(image_h)))
            };

            stage =
                stage
                    .child(placed(&pair.converted))
                    .child(
                        // The original, clipped to everything left of the divider. Its
                        // child keeps full width so both sides stay in register.
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .h_full()
                            .w(px(divider))
                            .overflow_hidden()
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .w(px(view_w))
                                    .h(px(view_h))
                                    .child(placed(&pair.original)),
                            ),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left(px(divider - 1.))
                            .w(px(2.))
                            .h_full()
                            .bg(rgba(0xffffffcc)),
                    )
                    .child(
                        // Which side is which, pinned to the divider rather than to the
                        // window, so it stays true as the divider moves.
                        div()
                            .absolute()
                            .top(px(48.))
                            .left(px(divider - 76.))
                            .w(px(64.))
                            .flex()
                            .justify_end()
                            .child(compare_chip("original", cx.theme().foreground, cx)),
                    )
                    .child(div().absolute().top(px(48.)).left(px(divider + 12.)).child(
                        compare_chip(self.format.label().to_uppercase(), cx.theme().green, cx),
                    ));
        }

        if comparison.failed {
            stage = stage.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Alert::error(
                            "compare-error",
                            "Could not build a comparison preview for this image.",
                        )
                        .max_w(px(420.)),
                    ),
            );
        }

        // Chrome as two full-width bars. These were black boxes pinned at
        // hand-computed offsets, the right-hand one at `view_w - 240` — a number
        // that stopped being the right edge the moment the text or window changed.
        let (saving_text, saving_colour) = match comparison.pair.as_ref() {
            Some(pair) => {
                let saving = pair.saving_percent(source_bytes);
                if saving >= 0. {
                    (format!("−{saving:.0}%"), cx.theme().green)
                } else {
                    (format!("+{:.0}%", -saving), cx.theme().yellow)
                }
            }
            None => (String::new(), cx.theme().green),
        };

        stage
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .bg(rgba(0x000000bf))
                    .text_size(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(cx.theme().foreground)
                            .font_weight(FontWeight::MEDIUM)
                            .child(name.clone()),
                    )
                    .children(comparison.pair.as_ref().map(|pair| {
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_shrink_0()
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .child(format!(
                                "{} → {} {} · {}",
                                format_bytes(source_bytes),
                                self.format.label().to_uppercase(),
                                self.quality.label(),
                                format_bytes(pair.converted_bytes)
                            ))
                            .child(compare_chip(saving_text, saving_colour, cx))
                    }))
                    .child(
                        Button::new("compare-close")
                            .small()
                            .icon(IconName::Close)
                            .tooltip("Back to the audit")
                            .on_click(cx.listener(|audit, _, _, cx| {
                                audit.compare = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .bg(rgba(0x000000bf))
                    .text_size(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .text_color(rgba(0xffffffcc))
                            .child(match (comparison.pair.as_ref(), scale) {
                                (Some(pair), Some(scale)) => {
                                    format!("{}×{} · {:.0}%", pair.width, pair.height, scale * 100.)
                                }
                                (None, _) if comparison.failed => "Preview unavailable".to_string(),
                                _ => "decoding…".to_string(),
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .flex_shrink_0()
                            .child(
                                Button::new("compare-fit")
                                    .ghost()
                                    .small()
                                    .label("Fit")
                                    .on_click(cx.listener(|audit, _, _, cx| {
                                        if let Some(comparison) = audit.compare.as_mut() {
                                            comparison.zoom = None;
                                            comparison.pan = (0., 0.);
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                Button::new("compare-actual")
                                    .ghost()
                                    .small()
                                    .label("100%")
                                    .on_click(cx.listener(|audit, _, _, cx| {
                                        if let Some(comparison) = audit.compare.as_mut() {
                                            comparison.zoom = Some(1.);
                                            comparison.pan = (0., 0.);
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                Button::new("compare-prev")
                                    .ghost()
                                    .small()
                                    .icon(IconName::ArrowLeft)
                                    .label("Prev")
                                    .on_click(cx.listener(|audit, _, _, cx| {
                                        audit.step_compare(-1, cx);
                                    })),
                            )
                            .child(
                                Button::new("compare-next")
                                    .ghost()
                                    .small()
                                    .icon(IconName::ArrowRight)
                                    .label("Next")
                                    .on_click(cx.listener(|audit, _, _, cx| {
                                        audit.step_compare(1, cx);
                                    })),
                            )
                            .child(
                                div()
                                    .ml_1()
                                    .text_color(rgba(0xffffffcc))
                                    .whitespace_nowrap()
                                    .child("Scroll to zoom · drag to pan"),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// One labelled row of the settings form.
    fn settings_row(
        label: &'static str,
        input: gpui::Entity<InputState>,
        secret: bool,
        cx: &Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(110.))
                    .flex_shrink_0()
                    .text_size(px(12.))
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                Input::new(&input)
                    .small()
                    .when(secret, |field| field.mask_toggle()),
            )
    }

    /// A section heading plus its status line, if one has anything to say.
    fn settings_status(status: Option<(bool, String)>, cx: &Context<Self>) -> gpui::Div {
        match status {
            None => div(),
            Some((ok, message)) => div()
                .text_size(px(11.))
                .text_color(if ok { cx.theme().green } else { cx.theme().red })
                .child(message),
        }
    }

    /// The settings panel: the CDN keys.
    fn settings_panel_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(panel) = self.settings_panel.as_ref() else {
            return div().into_any_element();
        };

        div()
            .w(px(480.))
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .rounded_lg()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_family("SF Pro Display")
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child("Sirv account"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child("Credentials stay on this computer."),
                    ),
            )
            .child(Self::settings_row(
                "Client ID",
                panel.client_id.clone(),
                false,
                cx,
            ))
            .child(Self::settings_row(
                "Client secret",
                panel.client_secret.clone(),
                true,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(Self::settings_status(panel.cdn_status.clone(), cx))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("settings-close")
                                    .ghost()
                                    .small()
                                    .label("Close")
                                    .on_click(cx.listener(|audit, _, _, cx| {
                                        audit.settings_panel = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("settings-save")
                                    .primary()
                                    .small()
                                    .label("Save credentials")
                                    .on_click(
                                        cx.listener(|audit, _, _, cx| audit.save_sirv_settings(cx)),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The remote-folder browser: a small panel over the window. Walk folders
    /// down, pair the folder you land on, or undo a pairing.
    fn sirv_browser_view(&self, browser: &SirvBrowser, cx: &mut Context<Self>) -> gpui::AnyElement {
        let paired = self
            .sirv_pairing
            .as_ref()
            .map(|pairing| pairing.dir.clone());

        let body: gpui::AnyElement = match browser.nodes.as_ref() {
            None => div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(12.))
                .text_color(cx.theme().muted_foreground)
                .child(IconName::LoaderCircle)
                .child(format!("Listing {}…", browser.path))
                .into_any_element(),
            Some(Err(message)) => div()
                .text_size(px(12.))
                .text_color(cx.theme().yellow)
                .child(message.clone())
                .into_any_element(),
            Some(Ok(nodes)) => {
                let mut rows: Vec<gpui::AnyElement> = Vec::new();
                if browser.path != "/" {
                    rows.push(
                        Button::new("sirv-up")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowUp)
                            .label("..")
                            .on_click(cx.listener(|audit, _, _, cx| audit.ascend_sirv(cx)))
                            .into_any_element(),
                    );
                }
                for (ix, node) in nodes.iter().filter(|node| node.is_folder()).enumerate() {
                    let name = node
                        .filename
                        .rsplit('/')
                        .next()
                        .unwrap_or(&node.filename)
                        .to_string();
                    let descend_to = name.clone();
                    rows.push(
                        Button::new(("sirv-dir", ix))
                            .ghost()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label(name)
                            .on_click(cx.listener(move |audit, _, _, cx| {
                                audit.descend_sirv(descend_to.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
                if rows.is_empty() {
                    rows.push(
                        div()
                            .text_size(px(12.))
                            .text_color(cx.theme().muted_foreground)
                            .child("No subfolders.")
                            .into_any_element(),
                    );
                }
                div()
                    .id("sirv-list")
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_0p5()
                    .max_h(px(280.))
                    .overflow_y_scroll()
                    .children(rows)
                    .into_any_element()
            }
        };

        div()
            .w(px(440.))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .font_family("SF Pro Display")
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child("Sync with Sirv"),
                    )
                    .child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(browser.path.clone()),
                    ),
            )
            .child(body)
            .when_some(self.sirv_job.as_ref(), |panel, job| {
                panel.child(
                    div()
                        .text_size(px(11.))
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(if job.failures.is_empty() {
                            cx.theme().muted_foreground
                        } else {
                            cx.theme().yellow
                        })
                        .child(match (job.finished, job.kind) {
                            (false, SirvJobKind::Pull) => {
                                format!("Pulling {} of {}…", job.done, job.total)
                            }
                            (false, SirvJobKind::PullChanged) => {
                                format!("Taking from Sirv {} of {}…", job.done, job.total)
                            }
                            (false, SirvJobKind::Push) => {
                                format!("Pushing {} of {}…", job.done, job.total)
                            }
                            (false, SirvJobKind::PushChanged) => {
                                format!("Overwriting on Sirv {} of {}…", job.done, job.total)
                            }
                            (true, kind) => {
                                let verb = match kind {
                                    SirvJobKind::Pull => "Pulled",
                                    SirvJobKind::PullChanged => "Took from Sirv",
                                    SirvJobKind::Push => "Pushed",
                                    SirvJobKind::PushChanged => "Overwrote on Sirv",
                                };
                                let failures = if job.failures.is_empty() {
                                    String::new()
                                } else {
                                    format!(
                                        ", {} failed: {}",
                                        job.failures.len(),
                                        job.failures.join(", ")
                                    )
                                };
                                format!("{verb} {} of {}{failures}", job.done, job.total)
                            }
                        }),
                )
            })
            .child(
                div().flex().items_center().justify_between().child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .when_some(paired, |row, dir| {
                            let busy = self.sirv_busy();
                            let (to_push, changed, to_pull) = self.sirv_counts.unwrap_or((0, 0, 0));
                            row.child(
                                Button::new("sirv-pull")
                                    .outline()
                                    .small()
                                    .icon(IconName::ArrowDown)
                                    .label(format!("Pull {to_pull} missing"))
                                    .disabled(busy || to_pull == 0)
                                    .on_click(cx.listener(|audit, _, _, cx| audit.start_pull(cx))),
                            )
                            .child(
                                Button::new("sirv-push")
                                    .outline()
                                    .small()
                                    .icon(IconName::ArrowUp)
                                    .label(format!("Push {to_push} new"))
                                    .disabled(busy || to_push == 0)
                                    .on_click(cx.listener(|audit, _, _, cx| audit.start_push(cx))),
                            )
                            .when(changed > 0, |row| {
                                row.child(
                                    Button::new("sirv-push-changed")
                                        .ghost()
                                        .small()
                                        .label(format!("Overwrite {changed} on Sirv"))
                                        .disabled(busy)
                                        .on_click(cx.listener(|audit, _, _, cx| {
                                            audit.start_push_changed(cx);
                                        })),
                                )
                                .child(
                                    Button::new("sirv-pull-changed")
                                        .ghost()
                                        .small()
                                        .label(format!("Take {changed} from Sirv"))
                                        .disabled(busy)
                                        .on_click(cx.listener(|audit, _, _, cx| {
                                            audit.start_pull_changed(cx);
                                        })),
                                )
                            })
                            .child(
                                Button::new("sirv-unpair")
                                    .ghost()
                                    .small()
                                    .label(format!("Unpair {dir}"))
                                    .disabled(busy)
                                    .on_click(cx.listener(|audit, _, _, cx| {
                                        audit.unpair_sirv(cx);
                                    })),
                            )
                        }),
                ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("sirv-close")
                            .ghost()
                            .small()
                            .label("Close")
                            .on_click(cx.listener(|audit, _, _, cx| {
                                audit.sirv_browser = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("sirv-pair")
                            .primary()
                            .small()
                            .label("Pair this folder")
                            .disabled(!matches!(browser.nodes, Some(Ok(_))))
                            .on_click(cx.listener(|audit, _, _, cx| audit.pair_sirv(cx))),
                    ),
            )
            .into_any_element()
    }

    /// The resize presets, as one segmented control. `ButtonGroup` reports the index
    /// that was clicked, so the options are listed once and read back by position.
    fn resize_group(&self, cx: &mut Context<Self>) -> ButtonGroup {
        let options = MaxEdge::PRESETS;
        ButtonGroup::new("resize")
            .children(options.iter().map(|edge| {
                segment(
                    gpui::SharedString::from(edge.label()),
                    edge.label(),
                    self.max_edge == *edge,
                )
                .disabled(self.converting)
            }))
            .on_click(cx.listener(move |audit, clicked: &Vec<usize>, _, cx| {
                if audit.converting {
                    return;
                }
                let Some(edge) = clicked.first().and_then(|index| options.get(*index)) else {
                    return;
                };
                audit.max_edge = *edge;
                audit.results.clear();
                audit.schedule_estimate(cx);
                cx.notify();
            }))
    }

    fn format_group(&self, cx: &mut Context<Self>) -> ButtonGroup {
        let options = [Format::WebP, Format::Avif];
        ButtonGroup::new("format")
            .children(options.iter().map(|format| {
                segment(
                    format.label(),
                    format.label().to_uppercase(),
                    self.format == *format,
                )
                .disabled(self.converting)
            }))
            .on_click(cx.listener(move |audit, clicked: &Vec<usize>, _, cx| {
                if audit.converting {
                    return;
                }
                let Some(format) = clicked.first().and_then(|index| options.get(*index)) else {
                    return;
                };
                audit.format = *format;
                // Results describe the old format; keeping them would mislabel them.
                audit.results.clear();
                audit.schedule_estimate(cx);
                cx.notify();
            }))
    }

    /// Bytes of what is on screen. With a filter active the folder total would be
    /// describing files the list is not showing.
    fn visible_bytes(&self) -> u64 {
        self.visible_bytes
    }

    fn toolbar_button(
        &self,
        id: &'static str,
        text: &'static str,
        tooltip: &'static str,
        icon: IconName,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Button {
        Button::new(id)
            .small()
            .icon(icon)
            .label(text)
            .tooltip(tooltip)
            .on_click(cx.listener(move |audit, _, _, cx| on_click(audit, cx)))
    }

    /// Which folder this is, and how to get to another one.
    fn header(&self, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let folder = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string());

        let mut stats = if count == self.entries.len() {
            format!("{count} images · {}", format_bytes(self.visible_bytes()))
        } else {
            format!(
                "{count} of {} images · {}",
                self.entries.len(),
                format_bytes(self.visible_bytes())
            )
        };
        if self.skipped_raw > 0 {
            stats.push_str(&format!(" · {} camera raw skipped", self.skipped_raw));
        }
        // Three states, three sentences. A pairing whose walk is still running used to
        // read exactly like one whose walk failed: no Sirv text at all.
        match (&self.sirv_pairing, self.sirv_counts) {
            (Some(_), Some((to_push, changed, to_pull))) => stats.push_str(&format!(
                " · Sirv: {to_push} to push · {changed} differ · {to_pull} to pull"
            )),
            (Some(pairing), None) => stats.push_str(match pairing.files {
                Listing::Walking => " · Sirv: listing…",
                Listing::Failed(_) => " · Sirv: listing failed",
                Listing::Ready(_) => "",
            }),
            (None, _) => {}
        }

        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            // Identity and its two actions share the top-left corner: the name,
            // then the openers that replace it. The path and the count sit
            // underneath as metadata.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_w(px(220.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .font_family("SF Pro Display")
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(folder),
                            )
                            .child(
                                Button::new("open-folder")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Folder)
                                    .label("Open folder…")
                                    .disabled(self.converting)
                                    .on_click(cx.listener(|audit, _, _, cx| audit.pick(true, cx))),
                            )
                            .child(
                                Button::new("open-file")
                                    .small()
                                    .ghost()
                                    .icon(IconName::File)
                                    .label("Open image…")
                                    .disabled(self.converting)
                                    .on_click(cx.listener(|audit, _, _, cx| audit.pick(false, cx))),
                            )
                            .child(
                                // The sync entry point: opens the remote-folder
                                // browser, which is also where a pairing is undone.
                                Button::new("sirv-browser")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Globe)
                                    .label(match &self.sirv_pairing {
                                        Some(pairing) => pairing.dir.clone(),
                                        None => "Sirv…".into(),
                                    })
                                    .disabled(self.converting)
                                    .on_click(
                                        cx.listener(|audit, _, _, cx| audit.open_sirv_browser(cx)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(cx.theme().muted_foreground)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(self.root.display().to_string()),
                            )
                            .child(
                                div()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(px(11.))
                                    .text_color(cx.theme().muted_foreground)
                                    .whitespace_nowrap()
                                    .flex_shrink_0()
                                    .child(stats),
                            ),
                    ),
            )
            // The view toggle sits at the far end of the window: it changes how
            // the list below is drawn and nothing else.
            .child(
                self.toolbar_button(
                    "view-grid",
                    if self.grid { "List" } else { "Grid" },
                    if self.grid {
                        "Show the audit as a list"
                    } else {
                        "Show the images as a gallery"
                    },
                    if self.grid {
                        IconName::Menu
                    } else {
                        IconName::LayoutDashboard
                    },
                    cx,
                    |audit, cx| {
                        audit.grid = !audit.grid;
                        cx.notify();
                    },
                )
                .disabled(self.converting),
            )
            .child(
                // Icon-only: the one global surface, always in the same corner.
                Button::new("open-settings")
                    .small()
                    .ghost()
                    .icon(IconName::Settings)
                    .tooltip("Settings")
                    .on_click(cx.listener(|audit, _, window, cx| audit.open_settings(window, cx))),
            )
    }

    /// The three knobs that decide what a conversion produces, each under its own
    /// name and drawn as one control rather than a run of loose buttons.
    fn controls(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lossless = self.quality == Quality::LOSSLESS;
        let heavy = self
            .entries
            .iter()
            .filter(|entry| Finding::Heavy.holds(entry))
            .count();
        // Below this width the audit controls and conversion settings are two
        // intentional bands. Letting one long flex row break wherever it ran out of
        // room left Quality orphaned on a line of its own at the default window size.
        let stacked = f32::from(window.viewport_size().width) < 1060.;

        div()
            .flex()
            .flex_wrap()
            .when(stacked, |strip| strip.flex_col().items_start())
            .when(!stacked, |strip| strip.items_center())
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            // What the list shows, then what a conversion would do: the reading
            // order of the strip follows the order you use it.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        div().w(px(150.)).flex_shrink_0().child(
                            Input::new(&self.filter_input)
                                .small()
                                .cleanable(true)
                                .disabled(self.converting)
                                .prefix(IconName::Search),
                        ),
                    )
                    // The audit colours every row by weight per pixel and then asks
                    // you to find the heavy ones yourself.
                    .children((heavy > 0).then(|| {
                        self.finding_button(
                            Finding::Heavy,
                            IconName::TriangleAlert,
                            format!("{heavy} heavy"),
                            cx,
                        )
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(self.control_group("Resize", self.resize_group(cx), cx))
                    .child(self.control_group("Format", self.format_group(cx), cx))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_shrink_0()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(cx.theme().muted_foreground)
                                    .whitespace_nowrap()
                                    .child("Quality"),
                            )
                            .child(
                                div()
                                    .w(px(110.))
                                    .debug_selector(|| "quality-control".to_string())
                                    .when(self.converting, |rail| {
                                        rail.child(
                                            Progress::new("quality-locked")
                                                .value(self.quality.0.unwrap_or(100.))
                                                .color(cx.theme().primary)
                                                .h(px(6.)),
                                        )
                                    })
                                    .when(!self.converting, |slider| {
                                        slider.child(Slider::new(&self.quality_slider).horizontal())
                                    }),
                            )
                            .child(
                                div()
                                    .w(px(26.))
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(px(12.))
                                    .whitespace_nowrap()
                                    .text_color(if lossless {
                                        cx.theme().muted_foreground
                                    } else {
                                        cx.theme().foreground
                                    })
                                    .child(match self.quality.0 {
                                        Some(value) => format!("{}", value.round() as u32),
                                        None => "—".to_string(),
                                    }),
                            )
                            .child(
                                Switch::new("lossless")
                                    .checked(lossless)
                                    .label("Lossless")
                                    .disabled(self.converting)
                                    .on_click(cx.listener(|audit, _, _, cx| {
                                        if audit.converting {
                                            return;
                                        }
                                        // A second click on a lit toggle has to turn it off,
                                        // or lossless is a one-way door.
                                        audit.quality = if audit.quality == Quality::LOSSLESS {
                                            Quality::lossy(audit.slider_quality)
                                        } else {
                                            Quality::LOSSLESS
                                        };
                                        audit.results.clear();
                                        audit.schedule_estimate(cx);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
    }

    /// The payoff, said once and out loud: what the folder costs now, what it would
    /// cost converted, and the button that does it. This used to be 11px of grey
    /// wedged between the button and the window edge — the wrong volume for the only
    /// number the app exists to produce.
    fn summary(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let target_count = self.target_count();
        // Source bytes only appear before a conversion. While results stream in,
        // avoid walking thousands of rows on every progress redraw.
        let source = if !self.converting && self.results.is_empty() {
            self.target_bytes()
        } else {
            0
        };

        // Four states, one shape: a headline, the share it leaves behind, and a
        // sentence of detail.
        let (headline, tone, detail, bar, tag) = if self.converting {
            let done = self.results.len() + self.failures.len();
            let total = self.active_target_count.unwrap_or(target_count);
            (
                format!("{done} of {total}"),
                cx.theme().foreground,
                format!(
                    "Converting to {} {}…",
                    self.format.label().to_uppercase(),
                    self.quality.label()
                ),
                Some((done as f32 / total.max(1) as f32, cx.theme().primary)),
                None,
            )
        } else if !self.results.is_empty() {
            let (before, after) = self.converted_totals();
            let growth = after > before;
            let delta = before.abs_diff(after);
            let percent = delta as f32 / before.max(1) as f32 * 100.;
            (
                format!(
                    "{} {}",
                    format_bytes(delta),
                    if growth { "larger" } else { "saved" }
                ),
                if growth {
                    cx.theme().yellow
                } else {
                    cx.theme().green
                },
                format!(
                    "{} converted · {} → {}",
                    self.results.len(),
                    format_bytes(before),
                    format_bytes(after)
                ),
                Some((
                    after as f32 / before.max(1) as f32,
                    if growth {
                        cx.theme().yellow
                    } else {
                        cx.theme().green
                    },
                )),
                Some((growth, percent)),
            )
        } else if let Some((projected, sampled)) = self.estimate {
            let growth = projected > source;
            let delta = source.abs_diff(projected);
            let percent = delta as f32 / source.max(1) as f32 * 100.;
            (
                // A projection from a few dozen encodes, said as one. Unqualified it
                // read as a measurement, and the reader had no way to tell it from the
                // completed total above, which is one.
                format!(
                    "≈{} to {}",
                    format_bytes(delta),
                    if growth { "grow" } else { "save" }
                ),
                if growth {
                    cx.theme().yellow
                } else {
                    cx.theme().green
                },
                format!(
                    "{} now → ≈{} as {} {} · sampled {sampled}",
                    format_bytes(source),
                    format_bytes(projected),
                    self.format.label().to_uppercase(),
                    self.quality.label()
                ),
                Some((
                    projected as f32 / source.max(1) as f32,
                    if growth {
                        cx.theme().yellow
                    } else {
                        cx.theme().green
                    },
                )),
                Some((growth, percent)),
            )
        } else {
            (
                "Sizing it up…".to_string(),
                cx.theme().muted_foreground,
                format!("{} on disk", format_bytes(source)),
                None,
                None,
            )
        };

        // A status bar: fixed at the bottom, one height in every state, so the
        // list above it never jumps when the numbers arrive.
        let (fraction, colour) = bar
            .map(|(remaining, colour)| (1. - remaining, colour))
            .unwrap_or((0., gpui::transparent_black()));

        div()
            .flex()
            .flex_col()
            .px_3()
            .pt_1()
            .pb_2()
            // The one strip allowed colour: washed in the tone of the headline,
            // so the state of the job reads before any word does.
            .bg(tone.opacity(0.08))
            .border_t_1()
            .border_color(cx.theme().border)
            .child(meter("saving", fraction, colour, 3.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .font_family("SF Pro Display")
                            .text_size(px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(tone)
                            .whitespace_nowrap()
                            .flex_shrink_0()
                            .child(headline),
                    )
                    // The share saved, which is the number people actually quote.
                    .children(tag.map(|(growth, percent)| {
                        let tag = if growth {
                            Tag::warning()
                        } else {
                            Tag::success()
                        };
                        tag.small().child(if growth {
                            format!("+{percent:.0}%")
                        } else {
                            format!("−{percent:.0}%")
                        })
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(detail),
                    )
                    .when(!self.selected.is_empty() && !self.converting, |row| {
                        row.child(
                            Button::new("select-none")
                                .ghost()
                                .small()
                                .label(format!("Clear {}", self.selected.len()))
                                .on_click(cx.listener(|audit, _, _, cx| {
                                    audit.selected.clear();
                                    audit.schedule_estimate(cx);
                                    cx.notify();
                                })),
                        )
                    })
                    .when(!self.results.is_empty() && !self.converting, |row| {
                        row.child(
                            Button::new("reveal")
                                .outline()
                                .small()
                                .icon(IconName::FolderOpen)
                                .label("Show output")
                                .on_click(cx.listener(|audit, _, _, _| audit.reveal_output())),
                        )
                    })
                    .child(
                        Button::new("convert")
                            .primary()
                            .when(self.converting || target_count == 0, |button| {
                                button.ghost()
                            })
                            .label(if self.converting {
                                "Converting…".to_string()
                            } else if self.selected.is_empty() {
                                format!("Convert all to {}", self.format.label().to_uppercase())
                            } else {
                                format!(
                                    "Convert {} to {}",
                                    target_count,
                                    self.format.label().to_uppercase()
                                )
                            })
                            .disabled(
                                self.converting || self.scanning.is_some() || target_count == 0,
                            )
                            .on_click(cx.listener(|audit, _, _, cx| audit.start_conversion(cx))),
                    ),
            )
    }

    /// Everything the scan could not take at face value, in one line rather than
    /// three scattered ones. The mislabelled count is a button: it is the audit's best
    /// finding, and a number you cannot act on is a dead end.
    fn notices(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let mut parts = Vec::new();
        if !self.unreadable.is_empty() {
            parts.push(format!(
                "would not decode: {}",
                named(self.unreadable.iter().filter_map(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                }))
            ));
        }
        if self.existing_output > 0 {
            parts.push(match self.existing_output {
                1 => format!("{}/ already holds 1 file", scan::OUTPUT_DIR),
                many => format!("{}/ already holds {many} files", scan::OUTPUT_DIR),
            });
        }
        if !self.failures.is_empty() {
            parts.push(format!("failed: {}", named(self.failures.iter().cloned())));
        }
        if let Some(pairing) = &self.sirv_pairing
            && let Listing::Failed(reason) = &pairing.files
        {
            parts.push(format!("could not list {}: {reason}", pairing.dir));
        }
        if let Some(job) = &self.sirv_job {
            let verb = match job.kind {
                SirvJobKind::Pull => "Sirv pull",
                SirvJobKind::PullChanged => "Sirv pull (overwrite)",
                SirvJobKind::Push => "Sirv push",
                SirvJobKind::PushChanged => "Sirv push (overwrite)",
            };
            let failures = if job.failures.is_empty() {
                String::new()
            } else {
                format!(
                    ", {} failed: {}",
                    job.failures.len(),
                    job.failures
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            parts.push(format!("{verb}: {} of {}{failures}", job.done, job.total));
        }
        if parts.is_empty() && self.mislabelled == 0 {
            return None;
        }

        // Left-aligned and only as wide as its text. A full-bleed box for six words
        // was a bigger shape on screen than the finding it was reporting.
        Some(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .children((self.mislabelled > 0).then(|| {
                    self.finding_button(
                        Finding::Mislabelled,
                        IconName::TriangleAlert,
                        match self.mislabelled {
                            1 => "1 file is not the format its extension claims".to_string(),
                            many => {
                                format!("{many} files are not the format their extension claims")
                            }
                        },
                        cx,
                    )
                }))
                .children((!parts.is_empty()).then(|| {
                    Alert::warning("notices", parts.join("  ·  "))
                        .icon(IconName::TriangleAlert)
                        .py_1()
                }))
                .into_any_element(),
        )
    }

    /// A finding shown as the control that narrows the list to it. Lit while it is the
    /// one in force, so the count and the list below it never disagree.
    fn finding_button(
        &self,
        finding: Finding,
        icon: IconName,
        label: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.finding == Some(finding);
        Button::new(("finding", finding as usize))
            .small()
            .icon(icon)
            .label(label)
            .selected(active)
            .when(!active, |button| button.ghost())
            .when(active, |button| button.warning())
            .on_click(cx.listener(move |audit, _, _, cx| audit.set_finding(finding, cx)))
    }
}

impl Render for Audit {
    // Three shapes share this method — empty state, comparison, and the list — so it
    // erases to one type rather than making the caller's `impl Trait` pick a winner.
    #[allow(refining_impl_trait)]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let count = self.visible.len();

        let title = match self.root.file_name() {
            Some(name) => format!("{} — ImageGuide", name.to_string_lossy()),
            None => "ImageGuide".to_string(),
        };
        if title != self.titled {
            window.set_window_title(&title);
            self.titled = title;
        }

        // Cheap enough to compare every frame, and it means a crash still leaves the
        // last good size and folder on disk. The write itself is delayed.
        let viewport = window.viewport_size();
        let current = settings::Settings {
            width: Some(f32::from(viewport.width)),
            height: Some(f32::from(viewport.height)),
            folder: self.root.is_dir().then(|| self.root.clone()),
        };
        if current != self.settings {
            self.remember_settings(current, cx);
        }

        if let Some(table) = self.table.clone() {
            let width = f32::from(viewport.width);
            let show_result = !self.results.is_empty();
            let signature = (width.round().max(0.) as u32, show_result);
            if self.table_signature != Some(signature) {
                self.table_signature = Some(signature);
                cx.defer(move |cx| {
                    table.update(cx, |table, cx| {
                        table.delegate_mut().set_viewport_width(width, show_result);
                        table.refresh(cx);
                    });
                });
            }
        }

        if let Some(scanning) = self.scanning.as_ref() {
            let label = scanning.clone();
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .bg(cx.theme().background)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .w(px(420.))
                        .px_4()
                        .py_4()
                        .rounded_lg()
                        .bg(cx.theme().secondary)
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .font_family("SF Pro Display")
                                .text_size(px(18.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child(format!("Scanning {label}…")),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "The current folder stays untouched until the scan finishes.",
                                ),
                        ),
                )
                .on_drop(cx.listener(|audit, paths: &gpui::ExternalPaths, _, cx| {
                    if let Some(path) = paths.paths().first() {
                        audit.request_path(path.clone(), cx);
                    }
                }))
                .into_any_element();
        }

        if self.entries.is_empty() {
            let empty_folder = self.root.is_dir();
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .bg(cx.theme().background)
                .child(
                    // A panel rather than loose text, so the window has something in
                    // it and the drop target has an edge you can see.
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .w(px(400.))
                        .px_4()
                        .py_6()
                        .border_dashed()
                        .border_1()
                        .border_color(if self.drag_over {
                            cx.theme().drag_border
                        } else {
                            cx.theme().border
                        })
                        .child(
                            div()
                                .font_family("SF Pro Display")
                                .text_size(px(19.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child(if empty_folder {
                                    "No supported images found"
                                } else {
                                    "Audit a folder of images"
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(cx.theme().muted_foreground)
                                .text_center()
                                .child(if empty_folder {
                                    "This folder has no supported images. Choose another folder \
                                     or drop an image here."
                                } else {
                                    "Nothing is uploaded. Every file is read, resized and \
                                     re-encoded on this machine."
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .pt_2()
                                .child(
                                    Button::new("empty-folder")
                                        .primary()
                                        .icon(IconName::Folder)
                                        .label("Open folder…")
                                        .on_click(
                                            cx.listener(|audit, _, _, cx| audit.pick(true, cx)),
                                        ),
                                )
                                .child(
                                    Button::new("empty-file")
                                        .outline()
                                        .icon(IconName::File)
                                        .label("Open image…")
                                        .on_click(
                                            cx.listener(|audit, _, _, cx| audit.pick(false, cx)),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .pt_2()
                                .text_size(px(12.))
                                .text_color(cx.theme().muted_foreground.opacity(0.7))
                                .child("or drop one anywhere in this window"),
                        ),
                )
                .on_drag_move(cx.listener(
                    |audit, _: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                        if !audit.drag_over {
                            audit.drag_over = true;
                            cx.notify();
                        }
                    },
                ))
                .on_drop(cx.listener(|audit, paths: &gpui::ExternalPaths, _, cx| {
                    audit.drag_over = false;
                    if let Some(path) = paths.paths().first() {
                        audit.request_path(path.clone(), cx);
                    }
                }))
                .into_any_element();
        }

        if self.settings_panel.is_some() {
            let view = self.settings_panel_view(cx);
            // The click that opened the panel left focus on the button it
            // replaced; take focus next frame so typing lands in the first
            // field. Once only: after that the field with focus is whichever
            // one Tab or a click chose. Nothing else in the framework moves Tab
            // between inputs, so this panel cycles them itself.
            cx.defer_in(window, |audit, window, cx| {
                if let Some(panel) = audit.settings_panel.as_mut()
                    && !panel.focused
                {
                    panel.focused = true;
                    let handle = panel.client_id.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                }
            });
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(cx.theme().background)
                .on_key_down(
                    cx.listener(|audit, event: &gpui::KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "escape" => {
                                audit.settings_panel = None;
                                cx.notify();
                            }
                            "tab" => {
                                const FIELDS: usize = 2;
                                let direction = if event.keystroke.modifiers.shift {
                                    FIELDS - 1
                                } else {
                                    1
                                };
                                if let Some(panel) = audit.settings_panel.as_mut() {
                                    panel.focus_ix = (panel.focus_ix + direction) % FIELDS;
                                    let handle = [
                                        panel.client_id.read(cx).focus_handle(cx),
                                        panel.client_secret.read(cx).focus_handle(cx),
                                    ][panel.focus_ix]
                                        .clone();
                                    window.focus(&handle, cx);
                                }
                            }
                            _ => {}
                        }
                    }),
                )
                .child(view)
                .into_any_element();
        }

        if let Some(browser) = self.sirv_browser.take() {
            let view = self.sirv_browser_view(&browser, cx);
            self.sirv_browser = Some(browser);
            // The click that opened the browser left focus on the header
            // button it replaced, so Escape had nowhere to land. Same fix as
            // the comparison: take focus next frame, once this tree exists.
            cx.defer_in(window, |audit, window, cx| {
                if let Some(browser) = audit.sirv_browser.as_ref() {
                    window.focus(&browser.focus, cx);
                }
            });
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(cx.theme().background)
                .track_focus(&self.sirv_browser.as_ref().unwrap().focus)
                .on_key_down(cx.listener(|audit, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        audit.sirv_browser = None;
                        cx.notify();
                    }
                }))
                .child(view)
                .into_any_element();
        }

        if let Some(comparison) = self.compare.take() {
            // Taken and put back so the view can borrow `self` immutably while the
            // listeners it builds hold a mutable handle to the same entity.
            let view = self.compare_view(&comparison, window, cx);
            self.compare = Some(comparison);
            // The click or Enter that opened this view left focus inside the list
            // it replaced, so Escape had nowhere to land. Take focus back next
            // frame, after the compare tree exists.
            cx.defer_in(window, |audit, window, cx| window.focus(&audit.focus, cx));
            return div()
                .size_full()
                .relative()
                .track_focus(&self.focus)
                .on_key_down(cx.listener(|audit, event: &gpui::KeyDownEvent, _, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => {
                            audit.compare = None;
                            cx.notify();
                        }
                        "right" | "down" => audit.step_compare(1, cx),
                        "left" | "up" => audit.step_compare(-1, cx),
                        "f" => {
                            if let Some(comparison) = audit.compare.as_mut() {
                                comparison.zoom = None;
                                comparison.pan = (0., 0.);
                                cx.notify();
                            }
                        }
                        "1" => {
                            if let Some(comparison) = audit.compare.as_mut() {
                                comparison.zoom = Some(1.);
                                comparison.pan = (0., 0.);
                                cx.notify();
                            }
                        }
                        _ => {}
                    }
                }))
                .child(view)
                .into_any_element();
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .track_focus(&self.focus)
            // Always bordered, so a hovering drag recolours the frame instead of
            // shifting the whole window's contents inward by two pixels.
            .border_2()
            .border_color(if self.drag_over {
                cx.theme().drag_border
            } else {
                gpui::transparent_black()
            })
            .on_drag_move(cx.listener(
                |audit, _: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                    if !audit.drag_over {
                        audit.drag_over = true;
                        cx.notify();
                    }
                },
            ))
            .on_key_down(
                cx.listener(|audit, event: &gpui::KeyDownEvent, window, cx| {
                    // The filter box swallows its own keys, so these only fire when the
                    // list itself has focus. Shift turns any move into a selection
                    // drag from the anchor.
                    let extend = event.keystroke.modifiers.shift;
                    match event.keystroke.key.as_str() {
                        "down" => audit.step_cursor(1, extend, cx),
                        "up" => audit.step_cursor(-1, extend, cx),
                        "left" => audit.step_cursor_lateral(-1, extend, cx),
                        "right" => audit.step_cursor_lateral(1, extend, cx),
                        "pagedown" => audit.step_cursor(10, extend, cx),
                        "pageup" => audit.step_cursor(-10, extend, cx),
                        "home" => audit.step_cursor(isize::MIN / 2, extend, cx),
                        "end" => audit.step_cursor(isize::MAX / 2, extend, cx),
                        "escape" => {
                            // Nothing is open here, so escape means "put the list down":
                            // the ticked set clears, the way it does in every file manager.
                            if !audit.selected.is_empty() && !audit.converting {
                                audit.selected.clear();
                                audit.schedule_estimate(cx);
                                cx.notify();
                            }
                        }
                        "a" if event.keystroke.modifiers.control
                            || event.keystroke.modifiers.platform =>
                        {
                            // Select what the list shows, not what the folder holds:
                            // a filter that hides files from the list must hide them
                            // from Convert too.
                            if !audit.converting {
                                audit.selected.extend(audit.visible.iter().copied());
                                audit.schedule_estimate(cx);
                                cx.notify();
                            }
                        }
                        "," if event.keystroke.modifiers.control
                            || event.keystroke.modifiers.platform =>
                        {
                            audit.open_settings(window, cx);
                        }
                        "space" => audit.toggle_cursor_selection(cx),
                        "enter" => {
                            if !audit.converting
                                && let Some(entry) = audit.entry_at(audit.cursor)
                            {
                                audit.open_compare(entry, cx);
                            }
                        }
                        _ => {}
                    }
                }),
            )
            .on_drop(cx.listener(|audit, paths: &gpui::ExternalPaths, _, cx| {
                audit.drag_over = false;
                if let Some(path) = paths.paths().first() {
                    audit.request_path(path.clone(), cx);
                }
            }))
            .child(self.header(count, cx))
            .child(self.controls(window, cx))
            .children(self.notices(cx))
            .child(
                // The list runs to the window edge; hairlines above it, not a
                // card floating in padding.
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_hidden()
                    .bg(cx.theme().table)
                    // Columns take a width, not a share, so the remainder after the
                    // fixed ones has to be handed to the name column by hand.
                    .child(if self.grid {
                        // One virtualised band is one row of fixed-size tiles.
                        let (root_left, root_right) = root_horizontal_chrome(window);
                        let layout = gallery_layout(
                            f32::from(window.viewport_size().width),
                            root_left,
                            root_right,
                            count,
                        );
                        if let Some(previous) = self.gallery_columns
                            && previous != layout.columns
                        {
                            self.gallery_scroll
                                .scroll_to_item_strict(0, ScrollStrategy::Top);
                        }
                        self.gallery_columns = Some(layout.columns);
                        let gallery = uniform_list(
                            "gallery",
                            layout.rows,
                            cx.processor(|audit, range: std::ops::Range<usize>, _window, cx| {
                                range
                                    .map(|band| {
                                        // A plain loop: the closure form borrows `audit`
                                        // mutably for `request_thumb` and immutably for
                                        // `tile`, which nested closures cannot express.
                                        let mut tiles = Vec::new();
                                        let (root_left, root_right) =
                                            root_horizontal_chrome(_window);
                                        let layout = gallery_layout(
                                            f32::from(_window.viewport_size().width),
                                            root_left,
                                            root_right,
                                            audit.visible.len(),
                                        );
                                        for row in layout.band_range(band) {
                                            let Some(entry) = audit.entry_at(row) else {
                                                continue;
                                            };
                                            audit.request_thumb(entry, cx);
                                            tiles.push(audit.tile(row, entry, layout.tile, cx));
                                        }
                                        div().flex().gap_2().children(tiles)
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.gallery_scroll)
                        .size_full()
                        .p_2();
                        div()
                            .relative()
                            .flex_1()
                            .overflow_hidden()
                            .child(gallery)
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .bottom_0()
                                    .w(Scrollbar::width())
                                    .debug_selector(|| "gallery-scrollbar".into())
                                    .child(
                                        Scrollbar::vertical(&self.gallery_scroll)
                                            .id("gallery-scrollbar")
                                            .mode(ScrollbarMode::Always)
                                            .viewport_from_layout(),
                                    ),
                            )
                            .into_any_element()
                    } else if let Some(table) = self.table.as_ref() {
                        DataTable::new(table)
                            .stripe(false)
                            .bordered(false)
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
            // The status bar sits at the very bottom, so nothing above it ever
            // changes height and the list never jumps.
            .child(self.summary(cx))
            .into_any_element()
    }
}
