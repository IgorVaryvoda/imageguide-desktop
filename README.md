# ImageGuide Desktop

Audit and optimise a folder of images without uploading them anywhere.

The conversion tools on [imageguide.dev](https://www.imageguide.dev) post your files
to a worker to do the work. That is fine for one screenshot and wrong for a client
shoot. This does the same job locally: nothing leaves the machine, and the folder size
is bounded by the disk rather than by a browser tab.

It is the desktop companion to the site and the
[Chrome extension](https://chromewebstore.google.com/detail/hinifcidioledficgenmdncpkifnngap).
The extension audits the images on a page and stops there, because a browser cannot
rewrite your files. This one can.

## Status

Audit, thumbnails, and WebP conversion all work.

```bash
imageguide ~/path/to/folder                        # audit, in a window
imageguide ~/photo.jpg                             # straight into the comparison
imageguide ~/path/to/folder --convert              # convert, no window
imageguide ~/path/to/folder --convert --quality 60
imageguide ~/path/to/folder --convert --lossless
```

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

Twelve mixed files from a real photo library, at q80:

```
12 converted at q80: 76.0 MB -> 4.6 MB, saved 71.4 MB (94%)
```

## Comparing

Click any row, or pass a single file, to open the original against the WebP the
current quality setting would produce. The encode happens in memory — nothing is
written, because the point is to decide whether the trade is acceptable *before*
committing to it.

**The view is 1:1.** Fitting a 5568px photo into a 900px window hides exactly the
artefacts the view exists to show, so both sides are drawn at native size, centred,
and cropped by the window. Move the pointer to sweep the divider across.

At q40 on a 12 MB photo the sky goes from grainy to smooth and the file goes to
262 KB. Whether that is a good trade is a judgement, which is why this shows you
rather than tells you.

## Planned

- Panning and zoom in the comparison. It is centred and 1:1 today, with no way to
  reach a corner of a large image.
- AVIF. Deferred: `rav1e` wants `nasm` to build with assembly, and it is worth doing
  once rather than badly.
- Spec profiles — "1400×1400, white background, under 250 KB" — for marketplace
  pre-flight.

## Build

```bash
cargo build --release   # fetches the pinned Rust toolchain on first run
cargo test
```

The UI is [GPUI](https://www.gpui.rs), pinned to a Zed revision because it has no
crates.io release and its API moves without notice.
