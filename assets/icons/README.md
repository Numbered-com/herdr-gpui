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

`herdr-ui-icon-clean.svg` and `.png` are the supplied rounded tile artwork;
`herdr-icon-square-clean.svg` and `.png` are the full-square variant.
The rounded PNG is used unchanged by the README and unbundled macOS runs;
Linux packages use the square PNG. On macOS, run `just icons` to regenerate
`Herdr.icns` from the rounded PNG
using Swift/CoreGraphics and Apple's `iconutil`, with high-quality downsampling
for each iconset resolution. This preserves the supplied artwork without a
custom SVG renderer or third-party tools. When changing
the SVG, also replace its 1024x1024 transparent PNG export before running the
generator. Assets are checked in, so ordinary builds do not require Swift.

The same generator uses CoreImage to map the supplied PNG's luminance to a red
palette, retaining transparency, producing `herdr-worktree-1024.png`,
`herdr-square-worktree-1024.png`, and `Herdr-worktree.icns`.
These derived assets identify linked-worktree builds;
the normal artwork remains unchanged. macOS development runs select the embedded
PNG at compile time, while macOS/Linux packaging reads the executable's build
identity to select the matching icon, even when packaging in another checkout.

`plus.svg` and `close.svg` are original tab-control artwork, reused by the
workspace menu alongside the original `pencil.svg` (rename), `trash.svg`
(delete checkout) and `chevron-up.svg` / `chevron-down.svg` (fold and unfold a
worktree group); `user.svg` is an
original generic silhouette for the future account placeholder, not a personal
identity or GitHub logo. All of them are embedded through a
minimal GPUI asset source. GPUI renders them as SVG masks tinted with the current
theme foreground, rather than fixed-color cached images.
