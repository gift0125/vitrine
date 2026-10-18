//! Chroma 主题：把设计稿的 Catppuccin Latte 色板灌进 gpui-component。
//!
//! 之前整个应用用的是 gpui-component 的默认中性灰主题（`Theme::default()`），
//! 和设计稿（Ardot file 737444551354649，Catppuccin Latte + Mauve 强调色）
//! 完全是两套东西——这就是"样式差别很大"的根因。这里把设计稿的 token 逐项
//! 映射过来，界面代码只读语义 token，不再出现任何写死的色值。
//!
//! 色板严格取 Catppuccin 官方值（Latte = 浅色，Mocha = 深色对应版）：
//!
//! | 角色 | Latte | 设计稿用途 |
//! |------|-------|-----------|
//! | Base | `#EFF1F5` | 窗口墙体、卡片、控制栏底 |
//! | Mantle | `#E6E9EF` | 次级面（输入框、悬停底） |
//! | Surface0 | `#CCD0DA` | 分隔线 |
//! | Surface1 | `#BCC0CC` | 描边、进度轨道 |
//! | Surface2 | `#ACB0BE` | 图标描边 |
//! | Text | `#4C4F69` | 标题/主文案 |
//! | Subtext1 | `#5C5F77` | 图标 |
//! | Overlay1 | `#8C8FA1` | 描述、编号、未激活 |
//! | Mauve | `#8839EF` | **唯一强调色**：播放按钮、进度已播放、当前播放项 |
//!
//! 设计稿明确「一个强调色」，所以 danger/success/warning 只用于语义反馈
//! （toast），不参与常规视觉。

use std::rc::Rc;

use gpui_kit::component::{Theme, ThemeConfig, ThemeMode};
use gpui_kit::{App, SharedString};

/// 便捷构造：`c("#8839EF")`。
fn c(hex: &'static str) -> Option<SharedString> {
    Some(hex.into())
}

/// 一套完整配色（Latte / Mocha 结构相同，只有色值不同）。
struct Palette {
    base: &'static str,
    mantle: &'static str,
    crust: &'static str,
    surface0: &'static str,
    surface1: &'static str,
    text: &'static str,
    subtext1: &'static str,
    overlay1: &'static str,
    mauve: &'static str,
    mauve_hover: &'static str,
    mauve_active: &'static str,
    /// Mauve 的极淡填充（当前播放行底色），8% 透明
    mauve_veil: &'static str,
    /// Mauve 的选中底色，20% 透明
    mauve_selection: &'static str,
    on_mauve: &'static str,
    red: &'static str,
    green: &'static str,
    yellow: &'static str,
    blue: &'static str,
}

const LATTE: Palette = Palette {
    base: "#EFF1F5",
    mantle: "#E6E9EF",
    crust: "#DCE0E8",
    surface0: "#CCD0DA",
    surface1: "#BCC0CC",
    text: "#4C4F69",
    subtext1: "#5C5F77",
    overlay1: "#8C8FA1",
    mauve: "#8839EF",
    mauve_hover: "#7726E0",
    mauve_active: "#6C1FD1",
    mauve_veil: "#8839EF14",
    mauve_selection: "#8839EF33",
    on_mauve: "#FFFFFF",
    red: "#D20F39",
    green: "#40A02B",
    yellow: "#DF8E1D",
    blue: "#1E66F5",
};

const MOCHA: Palette = Palette {
    base: "#1E1E2E",
    mantle: "#181825",
    crust: "#11111B",
    surface0: "#313244",
    surface1: "#45475A",
    text: "#CDD6F4",
    subtext1: "#BAC2DE",
    overlay1: "#7F849C",
    mauve: "#CBA6F7",
    mauve_hover: "#D7B8F9",
    mauve_active: "#B98CF5",
    mauve_veil: "#CBA6F71F",
    mauve_selection: "#CBA6F73D",
    on_mauve: "#11111B",
    red: "#F38BA8",
    green: "#A6E3A1",
    yellow: "#F9E2AF",
    blue: "#89B4FA",
};

/// 界面正文字体：挑**系统自带**的那一套。
///
/// 设计稿（Ardot）写的是 Inter / Noto Sans SC，这两套 Windows 都不预装——
/// 之前写死 `Noto Sans SC`，本机装过所以看着正常，但发行包到别人机器上
/// 字体解析失败会退到系统兜底：中文常常落到宋体，11px 下横细竖粗、笔画发糊，
/// 和设计稿的观感差得远。
///
/// 微软雅黑是 Windows 从 Vista 起自带的中文 UI 字体（`Microsoft YaHei UI` 是
/// 它的 UI 变体，字面更紧凑），资源管理器/设置页本身就是这套，中英混排最稳。
fn ui_font_family() -> &'static str {
    if cfg!(target_os = "windows") {
        "Microsoft YaHei UI"
    } else if cfg!(target_os = "macos") {
        "PingFang SC"
    } else {
        "Noto Sans SC"
    }
}

/// 等宽字体：时间码、序号用，必须真的等宽（雅黑不是）。
pub(crate) fn mono_font_family() -> &'static str {
    if cfg!(target_os = "windows") {
        "Consolas"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    }
}

/// 把一套色板翻译成 gpui-component 的语义 token。
///
/// 没有列出的 token 会回落到该模式的默认中性色，所以这里只覆盖界面真正
/// 用到的那些；后续换肤也只需要动这张表。
///
/// 注意：`ThemeConfigColors` 里的 base.* 色板字段是私有的，不能用结构体字面量
/// 构造，只能先 `default()` 再逐字段赋值。
fn config(name: &str, mode: ThemeMode, p: &Palette) -> ThemeConfig {
    let mut cfg = ThemeConfig {
        name: name.into(),
        mode,
        // 设计稿：正文 13px / 辅助 11px / 标题 15px，基准字号 16 让 rem 阶梯对齐
        font_family: Some(ui_font_family().into()),
        font_size: Some(16.0),
        mono_font_family: Some(mono_font_family().into()),
        mono_font_size: Some(12.0),
        // 设计稿：条目/卡片圆角 8，大面板 12
        radius: Some(8),
        radius_lg: Some(12),
        // 设计稿是纯平面层级，没有按钮投影
        shadow: Some(false),
        ..Default::default()
    };

    let col = &mut cfg.colors;

    // ---- 基础面 ----
    col.background = c(p.base);
    col.foreground = c(p.text);
    // Surface0：设计稿里唯一的分隔线色
    col.border = c(p.surface0);
    col.window_border = c(p.surface0);
    col.muted = c(p.mantle);
    col.muted_foreground = c(p.overlay1);
    col.secondary = c(p.mantle);
    col.secondary_foreground = c(p.subtext1);
    col.secondary_hover = c(p.crust);
    col.secondary_active = c(p.surface0);
    col.input = c(p.surface1);
    col.popover = c(p.base);
    col.popover_foreground = c(p.text);
    col.group_box = c(p.base);
    col.group_box_foreground = c(p.text);
    col.group_box_title_foreground = c(p.text);

    // ---- 强调色：Mauve 独家 ----
    col.primary = c(p.mauve);
    col.primary_foreground = c(p.on_mauve);
    col.primary_hover = c(p.mauve_hover);
    col.primary_active = c(p.mauve_active);
    // ghost 按钮悬停底：设计稿没有色块，用极淡的 Crust
    col.accent = c(p.crust);
    col.accent_foreground = c(p.text);
    col.ring = c(p.mauve);
    col.caret = c(p.mauve);
    col.selection = c(p.mauve_selection);
    col.link = c(p.mauve);
    col.link_hover = c(p.mauve_hover);
    col.link_active = c(p.mauve_active);

    // ---- 主按钮（播放按钮） ----
    col.button_primary = c(p.mauve);
    col.button_primary_foreground = c(p.on_mauve);
    col.button_primary_hover = c(p.mauve_hover);
    col.button_primary_active = c(p.mauve_active);
    col.button = c(p.mantle);
    col.button_foreground = c(p.text);
    col.button_hover = c(p.crust);
    col.button_active = c(p.surface0);

    // ---- 播放列表 ----
    col.list = c(p.base);
    col.list_hover = c(p.mantle);
    col.list_active = c(p.mauve_veil);
    col.list_active_border = c(p.mauve);
    col.list_head = c(p.base);
    col.list_even = c(p.base);

    // ---- 进度/音量 ----
    // Slider 的 bg 即"已播放段"，轨道是它的 20% 透明度版本
    col.slider_bar = c(p.mauve);
    col.slider_thumb = c(p.mauve);
    col.progress_bar = c(p.mauve);
    col.switch = c(p.surface1);
    col.switch_thumb = c(p.base);

    // ---- 滚动条 ----
    col.scrollbar = c(p.base);
    col.scrollbar_thumb = c(p.surface1);
    col.scrollbar_thumb_hover = c(p.surface1);

    // ---- 标题栏 ----
    col.title_bar = c(p.base);
    col.title_bar_border = c(p.surface1);
    col.status_bar = c(p.base);
    col.status_bar_border = c(p.surface0);

    // ---- 交互反馈（toast） ----
    col.danger = c(p.red);
    col.danger_foreground = c(p.base);
    col.danger_hover = c(p.red);
    col.danger_active = c(p.red);
    col.success = c(p.green);
    col.success_foreground = c(p.base);
    col.success_hover = c(p.green);
    col.success_active = c(p.green);
    col.warning = c(p.yellow);
    col.warning_foreground = c(p.base);
    col.warning_hover = c(p.yellow);
    col.warning_active = c(p.yellow);
    col.info = c(p.blue);
    col.info_foreground = c(p.base);
    col.info_hover = c(p.blue);
    col.info_active = c(p.blue);
    col.drag_border = c(p.mauve);
    col.drop_target = c(p.mauve_veil);

    cfg
}

/// 安装 Chroma 主题（浅色 Latte 为默认，深色 Mocha 备切换）。
///
/// 必须在 `gpui_kit::init(cx)` 之后调用：那一步会先装一份默认主题并
/// `Theme::change(Light)`，这里再把它替换成设计稿的色板。
/// 浮在画面上的控件（OSC）配色。
///
/// 和 App 表面的 Latte 是**两套体系**：叠在任意亮度的画面/封面上时，浅色浮条既压不住
/// 亮场景、也会把暗场景吃掉。IINA 在浅色系统下 OSC 同样走深色玻璃，这里照此处理；
/// 唯一贯穿两套体系的是 Mauve。
///
/// 设计稿玻璃底是 `#0B0C10` 60% + backdrop blur 32。Windows 上没有 backdrop blur，
/// 60% 压在亮画面上文字会糊，所以这里加浓到 72%。
pub mod overlay {
    use gpui_kit::Hsla;

    /// 视频画布：比玻璃更深，和画面黑边融在一起
    pub fn canvas() -> Hsla {
        gpui_kit::rgb(0x08090C).into()
    }

    /// 玻璃底：`#0B0C10` @ 72%
    pub fn glass() -> Hsla {
        gpui_kit::rgba(0x0B0C10B8).into()
    }

    /// 玻璃描边：白 10%
    pub fn border() -> Hsla {
        gpui_kit::rgba(0xFFFFFF1A).into()
    }

    /// 浮层主图标/文字
    pub fn fg() -> Hsla {
        gpui_kit::rgb(0xD5D9E2).into()
    }

    /// 浮层次要文字（剩余时间、标题带）
    pub fn dim() -> Hsla {
        gpui_kit::rgb(0x8B909C).into()
    }

    /// 信息卡里的字段名（设计稿 `#A6ADC8`）。
    ///
    /// 比 `dim()` 亮一档：信息卡是**要读**的卡片，成对出现的 label 太暗就只剩
    /// 右边一列值，扫读时对不上号。
    pub fn label() -> Hsla {
        gpui_kit::rgb(0xA6ADC8).into()
    }

    /// 已播放进度 / 音量：Mocha 的 Mauve，在暗底上比 `#8839EF` 更亮
    pub fn accent() -> Hsla {
        gpui_kit::rgb(0xCBA6F7).into()
    }

    /// 浮层上的警示色：Mocha 的 Red。
    ///
    /// 暗玻璃上用浅色主题的 `danger`（#D20F39）只有 3:1 出头，读起来发闷；
    /// Mocha 的 #F38BA8 在 #0B0C10 上有 9:1，和 `accent` 同一档亮度。
    pub fn danger() -> Hsla {
        gpui_kit::rgb(0xF38BA8).into()
    }

    /// 播放键：仍是品牌 Mauve（设计稿明确浮层里只有它用这个色）
    pub fn play() -> Hsla {
        gpui_kit::rgb(0x8839EF).into()
    }
}

pub fn install(cx: &mut App, dark: bool) {
    let light = Rc::new(config("Chroma Latte", ThemeMode::Light, &LATTE));
    let dark_cfg = Rc::new(config("Chroma Mocha", ThemeMode::Dark, &MOCHA));

    Theme::update(cx, |theme| {
        // 先登记两套配置，再让 apply_config 把浅色应用上去
        theme.light_theme = light.clone();
        theme.dark_theme = dark_cfg;
        theme.apply_config(&light);

        // toast 的位置和宽度一并按本项目口径改掉。
        //
        // 标题栏是我们自绘的（`TitlebarOptions.appears_transparent`），gpui-component
        // 不知道它有多高，默认只让开 TITLE_BAR_HEIGHT + 16 = 50——正好贴着最小化/
        // 关闭按钮的下沿，看着像压在按钮上。这里让开「标题带 + 12」。
        // 宽度同理：默认 382 在 1040 宽的窗口里占 37%，像一块公告板。
        theme.notification.margins.top = gpui_kit::px(crate::player_view::TITLE_BAND + 12.);
        theme.notification.margins.right = gpui_kit::px(16.);
        theme.notification.width = gpui_kit::px(320.);
    });

    if dark {
        Theme::change(ThemeMode::Dark, None, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::ActiveTheme as _;
    use gpui_kit::Hsla;

    /// 颜色比较统一走 `rgb() -> Hsla`，两端同一条转换路径，避免浮点误差。
    fn hex(value: u32) -> Hsla {
        gpui_kit::rgb(value).into()
    }

    /// 主题必须真的落到全局状态上——否则界面又回到默认中性灰。
    #[gpui_kit::test]
    fn ui_font_is_a_system_font_not_a_design_tool_font(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install(cx, false);

            let theme = cx.theme();
            let family = theme.font_family.as_ref();
            // 设计稿里的 Inter / Noto Sans SC 都不是 Windows 预装字体：写死它们，
            // 发行包到用户机器上会退到系统兜底（中文常落宋体），观感直接崩。
            // 这条用例守的是「选系统自带的那一套」这个决定本身。
            if cfg!(target_os = "windows") {
                assert_eq!(family, "Microsoft YaHei UI", "Windows 上应当用微软雅黑");
            }
            assert!(
                !matches!(family, "Inter" | "Noto Sans SC"),
                "界面字体不能是设计稿专用字体（用户机器上不一定有）: {family}"
            );
            // 时间码必须等宽，雅黑不是
            assert_ne!(
                theme.mono_font_family.as_ref(),
                family,
                "等宽字体不能跟正文同族"
            );
        });
    }

    #[gpui_kit::test]
    fn install_applies_latte_palette(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install(cx, false);

            let theme = cx.theme();
            assert!(!theme.mode.is_dark(), "默认应为浅色 Latte");
            assert_eq!(theme.theme_name().as_ref(), "Chroma Latte");
            assert_eq!(
                theme.background,
                hex(0xEFF1F5),
                "窗口底色应为 Catppuccin Latte 的 Base"
            );
            assert_eq!(theme.primary, hex(0x8839EF), "强调色应为 Mauve");
            assert_eq!(theme.border, hex(0xCCD0DA), "分隔线应为 Surface0");
        });
    }

    #[gpui_kit::test]
    fn dark_mode_uses_mocha_palette(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install(cx, true);

            let theme = cx.theme();
            assert!(theme.mode.is_dark());
            assert_eq!(theme.theme_name().as_ref(), "Chroma Mocha");
            assert_eq!(theme.background, hex(0x1E1E2E), "深色应为 Mocha Base");
            assert_eq!(theme.primary, hex(0xCBA6F7), "深色强调色为 Mocha Mauve");
        });
    }
}
