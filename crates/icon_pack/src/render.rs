//! 图标包图标的 GPUI 绘制与资源。
//!
//! - 单色（mask）：`svg()` 按调用方给的颜色着色，SVG 字节由 [`IconPackAssets`] 按内容路径提供。
//! - 彩色（color）：按物理像素栅格化成位图（GPUI 资产缓存去重），高分屏不糊；栅格中留空，
//!   避免先闪一下别的图标。

use std::{
    borrow::Cow,
    collections::HashMap,
    future::Future,
    hash::{Hash, Hasher},
    sync::{Arc, LazyLock},
};

use gpui::{
    AnyElement, App, Asset, AssetSource, DevicePixels, Hsla, IntoElement as _, Pixels, RenderImage,
    SharedString, Styled as _, SvgSize, Window, div, img, size, svg,
};

use crate::{IconMode, IconSvg, PackIcon, registry::builtin_packs};

/// 图标包 SVG 的资源路径前缀。
pub(crate) const ASSET_PREFIX: &str = "icon-packs/";
/// 彩色图标栅格边长上限（物理像素）。
const MAX_RASTER: u32 = 512;

/// 绘制一个图标包图标：`dark` 选亮 / 暗变体，`color` 只作用于单色图标。
pub fn pack_icon(
    icon: &PackIcon,
    dark: bool,
    edge: Pixels,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let variant = icon.variant(dark);
    match icon.mode {
        IconMode::Mask => svg()
            .path(variant.path.clone())
            .flex_none()
            .size(edge)
            .text_color(color)
            .into_any_element(),
        IconMode::Color => {
            let physical =
                ((f32::from(edge) * window.scale_factor()).ceil() as u32).clamp(1, MAX_RASTER);
            let key = RasterKey {
                svg: variant.clone(),
                edge: physical,
            };
            match window.use_asset::<ColorIconAsset>(&key, cx) {
                Some(Ok(image)) => img(image).flex_none().size(edge).into_any_element(),
                // 栅格中 / 失败（内置与已校验的 SVG 不会失败）：保留占位，不闪别的图标。
                Some(Err(_)) | None => div().flex_none().size(edge).into_any_element(),
            }
        }
    }
}

#[derive(Clone)]
struct RasterKey {
    svg: IconSvg,
    edge: u32,
}

/// 路径即内容哈希：只按路径 + 尺寸取键，渲染热路径上不重复哈希整段 SVG。
impl Hash for RasterKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.svg.path.hash(state);
        self.edge.hash(state);
    }
}

enum ColorIconAsset {}

impl Asset for ColorIconAsset {
    type Source = RasterKey;
    type Output = Result<Arc<RenderImage>, SharedString>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        let renderer = cx.svg_renderer();
        async move {
            let parsed = renderer
                .parse_svg(source.svg.text.as_bytes())
                .map_err(|error| SharedString::from(error.to_string()))?;
            let edge = DevicePixels(source.edge as i32);
            renderer
                .render_parsed(&parsed, SvgSize::Size(size(edge, edge)))
                .map_err(|error| SharedString::from(error.to_string()))
        }
    }
}

/// 内置包全部 SVG：资源路径 → 文本。
static BUILTIN_SVGS: LazyLock<HashMap<SharedString, Arc<str>>> = LazyLock::new(|| {
    builtin_packs()
        .flat_map(|pack| {
            pack.icons()
                .flat_map(|(_, icon)| icon.svgs().cloned().collect::<Vec<_>>())
        })
        .map(|svg| (svg.path, svg.text))
        .collect()
});

/// 图标包拥有的资源（`icon-packs/<内容哈希>.svg`），composition root 并入应用 `AssetSource`。
pub struct IconPackAssets;

impl AssetSource for IconPackAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if !path.starts_with(ASSET_PREFIX) {
            return Ok(None);
        }
        Ok(BUILTIN_SVGS
            .get(path)
            .map(|text| Cow::Owned(text.as_bytes().to_vec())))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(BUILTIN_SVGS
            .keys()
            .filter(|asset| asset.starts_with(path))
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;

    use super::IconPackAssets;
    use crate::{FileKind, builtin_pack};

    #[test]
    fn every_builtin_svg_loads_by_content_path() -> gpui::Result<()> {
        for id in ["lucide", "material", "catppuccin", "thunder"] {
            let pack = builtin_pack(id).expect(id);
            let (_, icon) = pack
                .matched_icon("a.zip", FileKind::Archive)
                .expect("archive icon");
            for svg in icon.svgs() {
                let bytes = IconPackAssets.load(&svg.path)?.expect("asset");
                assert_eq!(bytes.as_ref(), svg.text.as_bytes());
            }
        }
        assert!(IconPackAssets.load("icons/file.svg")?.is_none());
        Ok(())
    }
}
