//! 应用资源源：默认组件图标 + 播放器所需的额外 Lucide 图标。
//!
//! 默认 101 个组件图标（Play/Pause/Moon/Sun 等）由 `Assets` 提供；
//! SkipBack/SkipForward/Volume 系列等目录图标通过 `icon_assets!`
//! 在编译期精确嵌入，避免引入全部 1830 个 SVG。

use std::borrow::Cow;

use gpui_kit::assets::{icon_assets, Assets as ComponentAssets};
use gpui_kit::{AssetSource, Result, SharedString};

// 播放器控制条与歌单面板所需、且不在默认 101 个图标内的目录图标
icon_assets!(
    PlayerExtraIcons,
    [
        ListMusic,
        Music,
        SkipBack,
        SkipForward,
        Volume1,
        Volume2,
        VolumeX
    ]
);

/// 组合资源源：先查额外图标，再回退到默认组件资源。
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        // 必须先查 PlayerExtraIcons：ComponentAssets 对未知路径会返回错误
        if let Some(bytes) = PlayerExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        ComponentAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = ComponentAssets.list(path)?;
        paths.extend(PlayerExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
