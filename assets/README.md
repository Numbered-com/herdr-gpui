# Repository Social Preview

`social-preview-light.png` and `social-preview-dark.png` are 1280x640 (2:1)
images for GitHub's repository social preview, which also feeds the link cards
rendered by X, Slack, and other Open Graph consumers. GitHub accepts one image
per repository; the light variant reads best against dark timelines, and the
dark variant shows the sidebar, tab strip, and split panes with the same
activity-dot semantics the app paints. Neither is embedded in the application.

The `.svg` files are the sources. Both are self-contained, so they inline the
ram path from `icons/herdr-icon-square-clean.svg` rather than referencing it;
update them together when the mark changes, and keep the `NOTICE` attribution
for the upstream artwork accurate. Regenerate the PNG exports with
[librsvg](https://wiki.gnome.org/Projects/LibRsvg):

```sh
rsvg-convert -w 1280 -h 640 assets/social-preview-light.svg -o assets/social-preview-light.png
rsvg-convert -w 1280 -h 640 assets/social-preview-dark.svg -o assets/social-preview-dark.png
```

Keep each export under GitHub's 1 MB social preview limit and upload it from
the repository's Settings > General > Social preview.
