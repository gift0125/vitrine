//! 验收探针：把界面的关键状态写成一个 JSON 小文件。
//!
//! ## 为什么需要它
//!
//! 上一代架构里「哪些地方是 chrome」有系统级的真值：主窗口的**窗口区域**被切成了
//! 画面之外的几块，验收脚本读 `GetWindowRgn` 就能问「这个点归主窗口还是归视频窗」。
//! 现在画面就在主窗口里画，没有区域可读了——但「胶囊到底在不在」「面板开没开」
//! 这些判断还得有据可查。
//!
//! 所以这里把**同一份布局**换个出口：`chrome_layout()` 用界面自己的常数算出
//! `chrome_rects`（和当年的区域列表逐像素同义），tick 里状态变了就落一小段 JSON。
//! 设 `SMP_UI_STATE=<路径>` 才写，平时零开销。
//!
//! 它不是自证清白：脚本里所有「有没有真的画出来」的判断仍然靠截图取样，
//! 这个文件只回答「应用认为自己处在什么状态」。
//!
//! ## 为什么还把 `chrome_rects` 留着
//!
//! 它已经从渲染管线上下来了，但仍然是**唯一一处**把「标题带 / 遮罩 / 胶囊 / 信息卡 /
//! 歌单」的位置写成数据的地方：单元测试靠它断言几何（胶囊 760×92、面板右沿对齐、
//! 淡出后整块让出），脚本靠它判断点击该落在哪。删了它，这些断言就只能逐像素截图。

use std::path::Path;

/// 一块矩形（逻辑像素）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// 圆角半径（外壳区域是矩形，留个字段给探针/测试看）
    pub round: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            x,
            y,
            w,
            h,
            round: 0.0,
        }
    }

    pub fn rounded(x: f32, y: f32, w: f32, h: f32, round: f32) -> Self {
        Self { x, y, w, h, round }
    }

    /// 四周外扩（抗锯齿留白：贴边的白描边会被切掉半个像素）。
    pub fn inflate(self, pad: f32) -> Self {
        Self {
            x: self.x - pad,
            y: self.y - pad,
            w: self.w + pad * 2.0,
            h: self.h + pad * 2.0,
            round: self.round + pad,
        }
    }

    // 只有单元测试与验收语义用到（bin 构建里没有调用点）
    #[allow(dead_code)]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x <= self.x + self.w && y <= self.y + self.h
    }
}

/// 视频模式下压在画面上的那几块（逻辑像素，已含抗锯齿留白）。
///
/// 由 `PlayerView::chrome_layout()` 填——常数在视图那边（和 `render_*` 共用一套）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChromeLayout {
    /// 控制层是否可见（淡到 0 时整块让出，见 `CHROME_HIDDEN_EPS`）
    pub visible: bool,
    pub width: f32,
    pub height: f32,
    /// 标题带整条（右端有窗口按钮，中间不能截断）
    pub title_band: Option<Rect>,
    /// 底部 240 的渐隐遮罩（压在画面上，整条都归它）
    pub vignette: Option<Rect>,
    /// 悬浮胶囊
    pub capsule: Option<Rect>,
    /// `ⓘ` 视频信息卡
    pub info_card: Option<Rect>,
    /// 播放列表面板
    pub panel: Option<Rect>,
}

impl ChromeLayout {
    pub fn chrome_rects(&self) -> Vec<Rect> {
        if !self.visible {
            return Vec::new();
        }
        let mut rects = Vec::with_capacity(5);
        if let Some(rect) = self.title_band {
            rects.push(rect);
        }
        if let Some(rect) = self.vignette {
            rects.push(rect);
        }
        if let Some(rect) = self.capsule {
            rects.push(rect);
        }
        if let Some(rect) = self.info_card {
            rects.push(rect);
        }
        if let Some(rect) = self.panel {
            rects.push(rect);
        }
        rects
    }

    #[allow(dead_code)]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.chrome_rects().iter().any(|rect| rect.contains(x, y))
    }
}

/// 探针写出去的全部状态。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiState {
    /// 是否在视频模式
    pub video: bool,
    /// 控制层透明度（0~1）
    pub chrome_alpha: f32,
    pub panel_open: bool,
    pub info_open: bool,
    pub cinema: bool,
    /// 窗口尺寸（逻辑像素）
    pub window: (f32, f32),
    /// 视频舞台（画面区）的位置尺寸
    pub stage: Option<Rect>,
    /// 当前帧的像素尺寸（0,0 = 还没有画面）
    pub frame: (usize, usize),
    pub layout: ChromeLayout,
}

impl UiState {
    pub fn to_json(&self) -> String {
        let rects: Vec<String> = self
            .layout
            .chrome_rects()
            .iter()
            .map(|r| {
                format!(
                    "{{\"x\":{:.1},\"y\":{:.1},\"w\":{:.1},\"h\":{:.1},\"r\":{:.1}}}",
                    r.x, r.y, r.w, r.h, r.round
                )
            })
            .collect();
        let stage = match self.stage {
            Some(r) => format!(
                "{{\"x\":{:.1},\"y\":{:.1},\"w\":{:.1},\"h\":{:.1}}}",
                r.x, r.y, r.w, r.h
            ),
            None => "null".to_string(),
        };
        format!(
            "{{\"video\":{},\"chrome\":{:.3},\"panel\":{},\"info\":{},\"cinema\":{},\
             \"window\":[{:.1},{:.1}],\"stage\":{stage},\"frame\":[{},{}],\"rects\":[{}]}}",
            self.video,
            self.chrome_alpha,
            self.panel_open,
            self.info_open,
            self.cinema,
            self.window.0,
            self.window.1,
            self.frame.0,
            self.frame.1,
            rects.join(",")
        )
    }

    /// 写盘（先写临时文件再改名：读的一方永远看不到半截 JSON）。
    pub fn publish(&self, path: &Path) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, self.to_json()).is_err() {
            return;
        }
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(visible: bool) -> ChromeLayout {
        ChromeLayout {
            visible,
            width: 1280.0,
            height: 720.0,
            title_band: Some(Rect::new(0.0, 0.0, 1280.0, 40.0)),
            vignette: Some(Rect::new(0.0, 480.0, 1280.0, 240.0)),
            capsule: Some(Rect::rounded(260.0, 604.0, 760.0, 92.0, 14.0)),
            info_card: None,
            panel: None,
        }
    }

    /// 五块 chrome 的顺序与内容：标题带 / 遮罩 / 胶囊。
    #[test]
    fn chrome_rects_lists_the_visible_chrome() {
        let rects = layout(true).chrome_rects();
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0].h, 40.0, "第一条是标题带");
        assert_eq!(rects[1].h, 240.0, "第二条是底部遮罩");
        assert_eq!(rects[2].w, 760.0, "第三条是胶囊");
    }

    /// 控制层淡出之后一块都不留：这就是「画面整块让出去」的另一半。
    #[test]
    fn hidden_chrome_leaves_nothing() {
        assert!(layout(false).chrome_rects().is_empty());
        assert!(!layout(false).contains(640.0, 650.0));
        assert!(layout(true).contains(640.0, 650.0));
        // 画面中间那块本来就不归 chrome
        assert!(!layout(true).contains(640.0, 300.0));
    }

    /// JSON 里必须带 rects 数组（脚本靠它判断点击该落在哪）。
    #[test]
    fn json_has_rects_and_state() {
        let state = UiState {
            video: true,
            chrome_alpha: 1.0,
            layout: layout(true),
            frame: (1280, 720),
            ..Default::default()
        };
        let json = state.to_json();
        assert!(json.contains("\"rects\":[{"), "{json}");
        assert!(json.contains("\"video\":true"));
        assert!(json.contains("\"frame\":[1280,720]"));
        // 必须能被解析回来（脚本用 `json.loads`）
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("JSON 应合法");
        assert_eq!(parsed["rects"].as_array().unwrap().len(), 3);
    }
}
