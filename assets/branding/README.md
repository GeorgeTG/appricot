# Branding

APPricot's mark, wordmark, app icons and banners. These are the only approved
renditions; do not redraw the mark from the geometry below without regenerating
it from the SVG sources.

## The mark

An apricot seen from the front: a bold ring with a narrow opening at the top,
two leaves growing out of that opening, and a small application window drawn
inside the ring. The opening is where the stream comes out, and the leaves sit
in it; at rest the ring reads as closed, which is what keeps the silhouette
readable at 32 px. The window inside the ring is the product: a window of a
sandboxed desktop application, carried into a host page as its own window.

## Files

| File | Use |
|---|---|
| `appricot-mark-*.png` | The mark alone, on transparency. Sizes 1024, 512, 256, 128, 64, 32. |
| `appricot-icon-1024.png` … `-64.png` | The mark on a rounded dark plate. Use these for app icons, favicons and avatars; the plate keeps the white window glyph legible on any background. |
| `appricot-logo-horizontal-{dark,light}.png` | Mark and wordmark side by side. `-dark` is for dark backgrounds, `-light` for light ones. |
| `appricot-logo-stacked-{dark,light}.png` | Mark above a centred wordmark. Same `-dark`/`-light` rule. |
| `appricot-banner-1280x640.png` | Social preview card (GitHub/OpenGraph size). |
| `appricot-banner-1600x500.png` | Wide header banner. |
| `*.svg` | The vector sources every raster file above was rendered from. Edit these, then re-rasterise; do not edit the PNGs. |

The SVG sources use `DejaVu Sans` for the wordmark. It is the only bold
sans-serif every build container is guaranteed to have, and it is what the
shipped PNGs were rendered with; substituting another font changes the lockup
and needs a fresh export.

## Palette

| Role | Hex |
|---|---|
| Apricot, highlight | `#FFC776` |
| Apricot, primary | `#F79B2E` |
| Apricot, shadow | `#DE6B0A` |
| Leaf, light | `#6ECB68` |
| Leaf, dark | `#1D8A3C` |
| Ink on light | `#1B2429` |
| Ink on dark | `#F2F6F8` |
| Plate / banner base | `#28333B` to `#0C1114` |
| Muted text on dark | `#A9BAC3`, `#7C8D96` |

## Tagline

Banners carry **"Per-window application streaming"**, which is the product in
five words, and the line **"Sandboxed sessions · Damage-driven pixels · An
embeddable browser client"**, which restates principles 2, 4 and 5 of
[docs/vision.md](../../docs/vision.md). Do not invent copy for these assets;
any new line must come out of the vision and architecture documents first.

## Regenerating

Every PNG here was rasterised from the SVG next to it with GIMP 3.2.2 at the
size in its filename. To regenerate a size, load the SVG at the target size and
export it; there is no build step and none is planned.

## Licence

Same as the rest of the repository: MIT OR Apache-2.0
([ADR-0002](../../docs/adr/0002-licence.md)). The mark is drawn for APPricot and
has no other origin.
