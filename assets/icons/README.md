# Herdr Icon

Original artwork created for this repository, licensed under Apache-2.0 like
the application. No third-party logo, font, or stock artwork is used. The blue
branch-connected H on charcoal represents linked workspaces; transparent margins
keep the rounded tile aligned with other macOS Dock icons.

`herdr.svg` is the source of truth. On macOS, run `just icons` to regenerate
`herdr-1024.png` and `Herdr.icns` using Swift/CoreGraphics and Apple's `iconutil`.
The generator renders the SVG's deliberately small vocabulary (rounded rectangles,
round polylines, and circles) directly, without third-party tools. It fails on
unsupported elements. Each iconset resolution is rendered from vector geometry.
Generated assets are checked in, so ordinary builds do not require Swift.
