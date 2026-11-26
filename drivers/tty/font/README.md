# The console font

`spleen-8x16.psfu` is the 8×16 face of [Spleen](https://github.com/fcambus/spleen), a monospaced
bitmap font by Frederic Cambus. It is the upstream release file, byte for byte — nothing here was
generated, converted or edited.

| What        | Value                                                                           |
| ----------- | ------------------------------------------------------------------------------- |
| Upstream    | <https://github.com/fcambus/spleen>                                             |
| Release tag | `2.3.0` — <https://github.com/fcambus/spleen/releases/tag/2.3.0>                |
| Taken from  | `spleen-2.3.0.tar.gz`, member `spleen-2.3.0/spleen-8x16.psfu`                   |
| Tarball     | SHA-256 `7b5729aaa8378e9987eccdb98c644180a875dcc22ee7258d8be3791238b87a02`      |
| This file   | SHA-256 `d7f35dcccbc070fbb4b214583140d5998f04d1356ddb850721d143cf76f47c85`      |
| Size        | 10648 bytes                                                                     |
| Format      | PC Screen Font **version 1** (magic `36 04`), mode 3: 512 glyphs, Unicode table |
| Licence     | BSD-2-Clause — the upstream `LICENSE`, copied beside this file unchanged        |

To check the copy: `shasum -a 256 drivers/tty/font/spleen-8x16.psfu`.

## The format is PSF1, not PSF2

Upstream builds every size with `bdf2psf`, which writes PSF version 2 for the wider faces and
**version 1** for this one, the only face exactly 8 pixels wide. A PSF1 file has a four-byte header
(two magic bytes, a mode byte, the glyph height), then the glyphs at one byte per row with the most
significant bit leftmost, then a table of little-endian `u16` code points in which `0xFFFF` ends
each glyph's list.

`drivers/tty/src/font.rs` embeds the file with `include_bytes!` and checks that header in a `const`
context, so a different file — a PSF2 one included — fails the build rather than drawing garbage.
Its host tests walk the Unicode table and pin that glyph `n` is U+00`n` for every `n` in
`0x20..=0x7E`, which is what lets the renderer index the font by byte.

## Licence

Spleen is distributed under the BSD 2-Clause licence (`LICENSE` in this directory, which carries the
upstream copyright notice). minix.rs is BSD-3-Clause; the two are compatible — BSD-2-Clause asks
only that the copyright notice, the list of conditions and the disclaimer be kept with source and
binary redistributions, which keeping `LICENSE` beside the font does. The font keeps its own
licence; it is not relicensed.

## Updating

Replace the file with the one from a newer release tarball, update the tag and both digests above,
and run `cargo test -p minixrs-tty`. The golden-glyph test in `font.rs` is written out by hand and
will need a look if upstream redraws `A`.
