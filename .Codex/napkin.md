# Napkin

## Corrections
| Date | Source | What Went Wrong | What To Do Instead |
|------|--------|----------------|-------------------|
| 2026-08-20 | user | Treated the broken GPUI headless renderer as a reason to defer screenshot-backed UI proof | Launch the real pre-alpha app, control it through Hyprland/ydotool, capture with `grim`, and visually inspect every changed state and size |

## User Preferences
- Pull current changes and run the app end to end when requested.
- For pre-alpha UI work, reshape the design freely and make real app screenshots yourself; do not wait for an in-repo headless capture test.

## Patterns That Work
- `git pull --ff-only` followed by `cargo run --release` updates and launches the GPUI app; Cargo may fetch newly locked crates first.
- For real UI proof on Hyprland: launch `target/release/imageguide <folder>`, use `hyprctl` to focus/float/resize/move the `imageguide` window, interact with `hyprctl dispatch movecursor` plus `ydotool`/`wtype`, capture exact window geometry with `grim -g`, then inspect the PNG.
- If the desktop is locked, keep the lock intact: run the release app in headless Gamescope, capture its PipeWire node with GStreamer, and drive its Xwayland seat with `DISPLAY=:2 xdotool`. Use an isolated `XDG_CONFIG_HOME` for requested window sizes.
- In isolated Gamescope proof, `DISPLAY=:N xdotool key --window <gamescope-window> Down` then `space` verifies the list cursor and keyboard selection without touching the real desktop.
- The full non-intrusive proof recipe, verified 2026-08-22: `setsid env XDG_CONFIG_HOME=<temp> gamescope --backend headless -W 1100 -H 720 -- ./target/release/imageguide <folder> &`, read the node id from `node ID: N` in its log, then `gst-launch-1.0 -q pipewiresrc path=N num-buffers=10 ! videoconvert ! pngenc ! multifilesink location="f%03d.png"`. Drive it with `DISPLAY=:2 xdotool key/type/mousemove --window <id>`. The flag is `--backend headless`, not `--headless`.
- Do not `pkill` the app from the same Bash call that relaunches it: the kill takes the calling shell with it. Use one call to stop and a separate `setsid ... & disown` to start.
- `TableState` caches its column groups; after a viewport/result signature change, update the delegate and call `TableState::refresh` from `Context::defer`, not during `Audit::render`.
- During conversion, replace the `Slider` entity with the existing `Progress` primitive so the quality control is visibly locked and cannot receive accessibility actions; a 500-image Gamescope run exposed the active rail clearly.
- A status bar must hold one height in every state: render its meter always, colour it transparent when idle, or the list above jumps.
- GPUI key handlers only see keys that bubble through the focused element. When a view replaces the tree the click focused (list → comparison), defer `window.focus(&handle)` one frame or Escape lands nowhere.
- Per-side border colours do not exist in gpui's Styled; a row-level colour tick must be an absolute child of the row, not a border.
- Put row attributes on the row, not in the first cell: a rail inside the checkbox cell reads as checkbox decoration.
- Sirv readdir returns pretty-printed JSON; `lines().next()` on an error body yields a bare `{`. Keep the whole body, capped.
- parking_lot::Mutex is the house rule wherever `lock().unwrap()` would appear; gpui already ships it in the tree.
- When a PUT replaces a range that ends mid-construct, the leftover tail silently re-anchors to the next statement — after every structural edit, `cargo check` before the next edit, never batch two.
- Checkbox focus is nested under the audit root, so unmodified Space/Enter must stop at a wrapper `on_key_down`; otherwise the component toggles and the root cursor handler toggles again.
- An empty directory has `root.is_dir() == true` and no entries, so it must branch before the table rather than reuse the filter-empty copy.
- `bpp` was too cryptic in the real table; `B/px` fits the compact column and reads clearly in list and grid views.
- Comparison `pair == None` can mean either loading or a completed decode/encode failure; keep an explicit failed bit so the error panel and footer do not say `decoding…` forever.

## Patterns That Don't Work
- The documented ignored screenshot harness currently fails on this Linux host with `render_to_image not available: no HeadlessRenderer configured`; do not claim UI screenshot proof from `cargo test --bin imageguide -- --ignored screenshot` until the renderer setup is fixed.
- Do not persist `uniform_list` processor range as viewport state: GPUI also invokes the processor with a one-item measurement range. Read the tracked handle's public `base_handle.logical_scroll_top()` instead.
- A normal live app run writes its viewport and folder to `~/.config/imageguide/settings`; save or isolate that config before scripted resize tests, then restore it exactly.
- `DISPLAY=:N import -window <id>` against the nested Xwayland server returns a 235-byte grey rectangle. The app draws through Wayland and wgpu, so its pixels only exist on Gamescope's PipeWire node.
- A `cx.defer_in` focus grab written without a "once" guard runs on every render of the view that schedules it. In the settings panel that made Tab look broken: focus went back to the first field before the next keystroke arrived.

- Measure a performance idea before building on it. Decoding a JPEG at an eighth of its size sounded obvious and ran *slower*: `jpeg-decoder` is the only crate that offers scaling, it has no SIMD, and DCT scaling saves the inverse transform, not the Huffman pass that dominates. Reverted with the dependency.
- Launch the app for UI proof even when the tests pass. A notice reading "optimized/ already holds 5415 files" on a folder with no output directory exposed a real bug: `scan` skipped every path with an `optimized` component but counted them all, and those files were in a nested `Screenshots/optimized` no run would touch.
- Do not judge an estimator by reasoning about its bias. Convert a real folder to get ground truth, then sweep the sampling offset: the "obvious" fix (stratified slices) moved a −98% error to a −6% median but still swung between −53% and +59% at 16 slices. Only the sweep showed that 32 slices was the setting worth shipping.

## Domain Notes
- The desktop app binary is `target/release/imageguide`.
- The normal baseline is green: 36 tests pass, with the screenshot test ignored; clippy and `cargo fmt --check` also pass at `05384d3`.
- Benchmark folders: hardlink-mirror `~/Pictures` into `~/.cache/` (`/tmp` is tmpfs, so `ln` fails across filesystems). 5,732 images / 3.0 GB convert to 422.9 MB at WebP q80 full size.
- Measured on this 16-core host at `52520d2`: WebP conversion 54.3s serial vs 9.3s parallel for 255 MB; AVIF 128s serial, 88s at two files at once, 83s at four. rav1e already spends about 6 cores on a single image, so only WebP wants a worker per core.
- `ravif`'s `asm` and `threading` features are on by default and `nasm` is installed, so the rav1e assembly is already built. The old `convert.rs` module comment claiming otherwise was wrong.
