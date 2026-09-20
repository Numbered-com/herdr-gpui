use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

pub(super) struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            "icons/plus.svg" => include_bytes!("../../../assets/icons/plus.svg"),
            "icons/close.svg" => include_bytes!("../../../assets/icons/close.svg"),
            "icons/user.svg" => include_bytes!("../../../assets/icons/user.svg"),
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(["icons/plus.svg", "icons/close.svg", "icons/user.svg"]
            .into_iter()
            .filter(|name| name.starts_with(path))
            .map(Into::into)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use gpui::{DevicePixels, Image, ImageFormat, TestAppContext, size};

    #[gpui::test]
    fn embedded_icons_render_nonempty_masks(cx: &mut TestAppContext) {
        let renderer = cx.update(|cx| cx.svg_renderer());
        for path in ["icons/plus.svg", "icons/close.svg", "icons/user.svg"] {
            let bytes = Icons.load(path).unwrap().unwrap();
            // Decode through GPUI's SVG renderer; production uses svg() for tinting.
            let image = Image::from_bytes(ImageFormat::Svg, bytes.into_owned())
                .to_image_data(renderer.clone())
                .unwrap();
            assert_eq!(image.size(0), size(DevicePixels(24), DevicePixels(24)));
            let pixels = image.as_bytes(0).unwrap();
            assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] > 0));
            assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == 0));
        }
        assert!(Icons.load("unknown.svg").unwrap().is_none());
        assert_eq!(Icons.list("icons/").unwrap().len(), 3);
    }
}
