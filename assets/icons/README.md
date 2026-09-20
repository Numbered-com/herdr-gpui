# Herdr Icon

The application icon combines Herdr's ram and terminal prompt with an ivory
native application window. The ram path is adapted from
[Herdr's logo](https://github.com/herdrdev/herdr/blob/HEAD/assets/logo.svg),
licensed under Apache-2.0. Changes include placement, scaling, graphite shading,
shadows, and the surrounding window tile. See the root [NOTICE](../../NOTICE)
and [LICENSE](../../LICENSE) for distribution attribution and license terms.
Transparent margins keep the rounded tile aligned with other macOS Dock icons.

`herdr.svg` and `herdr-1024.png` are the supplied vector artwork and its raster
export. The PNG is used unchanged by the README, Linux packages, and unbundled
macOS runs. On macOS, run `just icons` to regenerate `Herdr.icns` from the PNG
using Swift/CoreGraphics and Apple's `iconutil`, with high-quality downsampling
for each iconset resolution. This preserves the supplied gradients, clipping,
and shadows without a custom SVG renderer or third-party tools. When changing
the SVG, also replace its 1024x1024 transparent PNG export before running the
generator. Assets are checked in, so ordinary builds do not require Swift.

The same generator uses CoreImage to map the supplied PNG's luminance to a red
palette, retaining transparency and shading, producing `herdr-worktree-1024.png`
and `Herdr-worktree.icns`. These derived assets identify linked-worktree builds;
the normal artwork remains unchanged. macOS development runs select the embedded
PNG at compile time, while macOS/Linux packaging reads the executable's build
identity to select the matching icon, even when packaging in another checkout.

`plus.svg` and `close.svg` are original tab-control artwork; `user.svg` is an
original generic silhouette for the future account placeholder, not a personal
identity or GitHub logo. All three are embedded through a
minimal GPUI asset source. GPUI renders them as SVG masks tinted with the current
theme foreground, rather than fixed-color cached images.
