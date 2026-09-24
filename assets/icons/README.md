# Herdr Icon

The application icons combine Herdr's ram and terminal prompt with flat ivory
and graphite colors, without window controls, gradients, or shadows. The ram path is adapted from
[Herdr's logo](https://github.com/herdrdev/herdr/blob/HEAD/assets/logo.svg),
licensed under Apache-2.0. Changes include placement, scaling, flat colors,
and the surrounding tile. See the root [NOTICE](../../NOTICE)
and [LICENSE](../../LICENSE) for distribution attribution and license terms.
Transparent margins keep the rounded tile aligned with other macOS Dock icons.

| macOS / README | Linux |
| --- | --- |
| <img src="herdr-ui-icon-clean.png" width="128" height="128" alt="Rounded Herdr icon"> | <img src="herdr-icon-square-clean.png" width="128" height="128" alt="Square Herdr icon"> |

`herdr-ui-icon-clean.svg` is the rounded tile source artwork;
`herdr-icon-square-clean.svg` is the full-square variant.
Their generated 1024x1024 PNG exports serve the README and About box
(rounded); Linux packages install the square SVG itself as the scalable icon.
On macOS, install the SVG renderer with
`brew install librsvg`, then run `just icons` after changing either SVG.
The generator uses `rsvg-convert` to rasterize the vector artwork directly at
each iconset resolution, rather than downsampling a PNG, and Apple's `iconutil`
to package `Herdr.icns`. It also regenerates the PNG exports.
Assets are checked in, so ordinary builds do not require Swift or librsvg.

### macOS sizing

The flattened `.icns` icons follow Apple's
[macOS Sequoia production template](https://devimages-cdn.apple.com/design/resources/download/macOS-Sequoia-Production-Templates-Sketch.dmg)
(`Template - Icon - App.sketch`). Its centered tile bounds are:

| Canvas (pixels) | Tile (pixels) | Margin on each side (pixels) |
| --- | --- | --- |
| 16 | 14 | 1 |
| 32 | 28 | 2 |
| 64 | 52 | 6 |
| 128 | 104 | 12 |
| 256 | 206 | 25 |
| 512 | 412 | 50 |
| 1024 | 824 | 100 |

The generator fits the source SVG's 896px rounded tile to these bounds before
rasterizing. Both the normal and red worktree icons use the same margins,
including the embedded 1024px PNG used by the About box. The full-square
Linux artwork uses its original canvas.

All five logical sizes (16, 32, 128, 256, 512) include 1x and Retina 2x
representations, as described in Apple's
[high-resolution iconset guidance](https://developer.apple.com/library/archive/documentation/GraphicsAnimation/Conceptual/HighResolutionOSX/Optimizing/Optimizing.html).
Run `swift scripts/check-icons.swift` on macOS to verify the committed PNGs and
every representation extracted from both `.icns` files.

These are precomposed icons for macOS 15 and later. The newer
[Icon Composer guidance](https://developer.apple.com/design/human-interface-guidelines/app-icons)
for full-bleed, unmasked layers applies to a different, layered icon pipeline.

The same generator uses CoreImage to map each rendered image's luminance to a red
palette, retaining transparency, producing `herdr-worktree-1024.png`,
`herdr-square-worktree-1024.png`, and `Herdr-worktree.icns`.
These derived assets identify linked-worktree builds;
the normal artwork remains unchanged. macOS development runs select the embedded
multi-resolution `.icns` at compile time for the Dock, while macOS/Linux packaging reads the executable's build
identity to select the matching icon, even when packaging in another checkout.

`plus.svg` and `close.svg` are original tab-control artwork, reused by the
workspace menu alongside the original `pencil.svg` (rename), `trash.svg`
(delete checkout) and `chevron-up.svg` / `chevron-down.svg` (fold and unfold a
worktree group); `git-branch.svg` is original artwork for the titlebar's Git
actions button; `user.svg` is an
original generic silhouette for the future account placeholder, not a personal
identity or GitHub logo. All of them are embedded through a
minimal GPUI asset source. GPUI renders them as SVG masks tinted with the current
theme foreground, rather than fixed-color cached images.
