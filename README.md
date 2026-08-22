# ImageGuide Desktop

Audit and optimise a folder of images without uploading them anywhere.

The conversion tools on [imageguide.dev](https://www.imageguide.dev) post your files
to a worker to do the work. That is fine for one screenshot and wrong for a client
shoot. This does the same job locally: nothing leaves the machine, and the folder size
is bounded by the disk rather than by a browser tab.

![The audit](docs/audit.webp)

It is the desktop companion to the site and the
[Chrome extension](https://chromewebstore.google.com/detail/hinifcidioledficgenmdncpkifnngap).
The extension audits the images on a page and stops there, because a browser cannot
rewrite your files. This one can.

## Status

Audit, thumbnails, and WebP conversion all work.

```bash
imageguide                                         # empty state: pick or drop
imageguide ~/path/to/folder                        # audit, in a window
imageguide ~/photo.jpg                             # straight into the comparison
imageguide ~/path/to/folder --convert              # convert to WebP, no window
imageguide ~/path/to/folder --convert --avif
imageguide ~/path/to/folder --convert --max-edge 1600
imageguide ~/path/to/folder --convert --quality 60
imageguide ~/path/to/folder --convert --lossless  # WebP only
```

Launched with no path it opens on an empty state: **Open folder…**, **Open image…**,
or drop either onto the window. The same two buttons sit in the toolbar afterwards,
so you can change folder without restarting. Picking a new one drops every thumbnail
and result belonging to the old one, because a stale saving next to a new file is a
lie.

It walks the folder and its subfolders, reads each image's header, and lists what it
found — heaviest first, because that is where the work is.

| Column | Meaning |
|---|---|
| Thumb | Decoded off the main thread, only for rows the viewport asked for |
| Format | The real format, read from the file's magic bytes, not its extension |
| Size | Pixel dimensions |
| bpp | Bytes on disk per output pixel |
| Weight | Bytes on disk |

**Format is read from the content.** That column disagreeing with the file extension
is a finding, not a display bug. The first folder this was pointed at —
`imageguide/public` — held 169 files named `.webp`, and 59 of them were PNG.

`bpp` is the quick read on whether a file is carrying weight it does not need. A
photographic JPEG sits near 0.2. A screenshot saved as PNG can be ten times that.

**Camera raw is counted, not listed.** `.nef`, `.cr2`, `.arw` and friends are TIFF
containers, so a header read returns the embedded preview — a 6000x4000 NEF reports
as a 160x120 TIFF and every derived number becomes a lie. They are also not web
delivery candidates. The header says how many were skipped rather than quietly
shortening the total.

The list is virtualised, and a row's thumbnail is decoded only once it has been on
screen. A folder of 6,000 images does not decode 6,000 files.

Reading headers only is deliberate. Decoding a 6000px JPEG to learn that it is 6000px
wide costs a hundred times what reading its header costs, and a shoot folder holds
thousands of them.

## Converting

Pick a quality in the header and press **Convert to WebP**, or use `--convert` to do
the same work without a window. Files are written to `optimized/` inside the folder,
mirroring its subfolder layout. Sources are never touched, and that output folder is
excluded from later scans so a second run does not offer to convert its own output.

Eight files encode at once. Each holds a fully decoded image in memory, so that
number is a memory bound as much as a CPU one.

**Anything with real transparency goes lossless** whatever quality you asked for.
libwebp's lossy path mangles alpha in ways that ruin cut-outs. An image with an alpha
channel that is entirely opaque is treated as opaque, because that is just an RGB
image paying for a fourth channel.

A file that grew is reported as grown rather than hidden. Re-encoding an
already-optimal JPEG usually costs bytes, and that is worth seeing.

### Size first

Most of the weight in a web image is its dimensions, not its format. Re-encoding a
6400px photo as AVIF still hands back a 6400px photo. The **full / 2400px / 1600px /
1000px** buttons cap the longest edge before encoding, and that single setting beats
every format change:

| Same twelve files, q80 | Result |
|---|---|
| WebP, full size | 76.0 MB → 4.6 MB (94%) |
| AVIF, full size | 76.0 MB → 4.1 MB (95%) |
| AVIF, 1600px | 76.0 MB → **1.1 MB (98%)** |

Downscaling is Lanczos3, not the fast filter used for thumbnails — this one gets
shipped, and a soft downscale wastes the bytes it saves. It never scales up: an 800px
source asked to fit 2000px is already inside the budget.

When a size budget is set, the comparison downscales the *original* side too. Holding
a 6400px source against a 1600px export would measure the resize, not the
compression, and the resize is not the part you need to eyeball.

### Two views

**Grid** switches the list for a gallery of tiles, virtualised the same way — a folder
of 5,700 images decodes only the tiles on screen. `--grid` opens straight into it.

### Before you convert

The toolbar carries a live projection: **≈ 3.1 MB · −96% (from 4)**. Four files are
encoded in memory, spread across the list rather than taken from the top, and the
ratio is applied to the whole job. It re-runs when the format, quality, size or
filter changes, after a short pause so dragging the slider does not start a run per
pixel.

It is a sample, and it says so — on the twelve-file folder above it projected 3.1 MB
against an actual 4.6 MB. Right about the order of the saving, not a promise about
the byte.

### Around the list

Quality is a slider from 1 to 100, with a separate lossless toggle. The box at the
top filters by filename, and narrowing the list narrows what Convert will touch —
converting files you cannot see would be a nasty surprise. Column headers sort. Clicking the active one reverses it; numeric columns open
largest-first and text columns A to Z. Ties fall back to the filename, so equal values
never reshuffle themselves between sorts.

The list takes the keyboard too: arrows and Page Up/Down move, Home and End jump,
Space ticks the row, Enter opens the comparison.

In the comparison, **Escape** closes and the **arrow keys** step to the next image,
which keeps the current format, quality, and size settings — that is the fast way to
sweep a folder for anything the encoder mangles.

The window title follows the folder, a progress bar runs during conversion, and the
tick box in the header row selects or clears everything. Window size and last folder
are remembered between launches. **Show output** appears once a run has written
something.

Failures are named, not counted. Files that will not decode are reported at the top
of the window, and so are the ones a conversion could not read or write — with the
first few filenames, because "3 failed" is not a report.

### Choosing what to convert

Tick rows to convert only those. With nothing ticked, Convert takes the whole folder,
so the common case needs no ticking. On a 5,733-image folder you usually want the top
twenty, which are already at the top.

### WebP or AVIF

64 size-stratified files from a real photo library, at q80 on a 16-thread machine:

| | Result | Wall clock |
|---|---|---|
| WebP | 134.1 MB → 5.4 MB (96%) | 0.67 s |
| AVIF, former rav1e encoder | 134.1 MB → 4.0 MB (97%) | 21.50 s |
| AVIF, current libaom encoder | 134.1 MB → **3.6 MB** (97%) | **9.07 s** |

The current path is 58% faster and 9% smaller than the former rav1e path at matched
visual quality. It uses the system libavif and libaom libraries directly, with libyuv
acceleration where packaged, so there is no subprocess per image.

AVIF has no lossless option here. Lossless AVIF is routinely larger than lossless
WebP and much slower to produce, so `--lossless` means WebP.

AVIF carries alpha in its own plane, so unlike WebP it needs no transparency special
case. A test encodes a half-transparent image and decodes it back, because a
regression there would silently flatten every cut-out.

## Comparing

Click any row, or pass a single file, to open the original against the WebP the
current quality setting would produce. The encode happens in memory — nothing is
written, because the point is to decide whether the trade is acceptable *before*
committing to it.

![Original against WebP, at 1:1](docs/comparison.webp)

**The view is 1:1.** Fitting a 5568px photo into a 900px window hides exactly the
artefacts the view exists to show, so both sides are drawn at native size, centred,
and cropped by the window.

Move the pointer to sweep the divider across. **Hold the left button and drag to
pan** — both sides move together, so they never fall out of register.

At q40 on a 12 MB photo the sky goes from grainy to smooth and the file goes to
262 KB. Whether that is a good trade is a judgement, which is why this shows you
rather than tells you.

## Planned

- Zoom in the comparison. It is 1:1 and pannable, but there is no way to back out to
  a whole-image view.
- Windows. dav1d decodes AVIF and libavif/libaom encode it; getting those onto a
  Windows runner means vcpkg. Ubuntu and macOS are both green in CI.
- Spec profiles — "1400×1400, white background, under 250 KB" — for marketplace
  pre-flight.

## Build

```bash
cargo build --release   # fetches the pinned Rust toolchain on first run
cargo test
```

Needs `dav1d` to decode AVIF and libavif with libaom to encode it. Linux packages
also provide libyuv for faster pixel conversion:

```bash
sudo pacman -S dav1d libavif aom libyuv                  # Arch and derivatives
sudo apt install libdav1d-dev libavif-dev libaom-dev libyuv-dev
brew install libavif                                      # macOS
```

Nothing in `src/` is platform-specific. Two dependencies are, and `Cargo.toml` splits
them by target: gpui's window backends (`wayland` and `x11` are Linux-only features,
and macOS and Windows pick their own), and `rfd`, whose xdg-portal backend keeps GTK
out of the Linux build while the other platforms use their native dialogs by default.

Only Linux is tested so far. The macOS and Windows builds are believed-correct, not
verified.

The UI is [GPUI](https://www.gpui.rs) with
[gpui-component](https://github.com/longbridge/gpui-component) for the widgets.
Neither has a crates.io release, and gpui-component tracks Zed's default branch with
no revision of its own — pinning one here would hand cargo two different git sources
for gpui and two incompatible copies of every type in it. `Cargo.lock` pins the
actual commits; CI builds `--locked`.

## Licence

MIT. See [LICENSE](LICENSE).

The two screenshots in this README were compressed by this tool — 354 KB of PNG to
71 KB of WebP at q82.
