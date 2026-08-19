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

Early. The audit works, with thumbnails. Conversion is not written yet.

```bash
cargo run --release -- ~/path/to/folder
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

## Planned

- WebP conversion with a real before/after size, written to an output folder.
- A full-resolution original-versus-converted slider. Judging compression is the
  whole job and it is what a browser is worst at.
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
