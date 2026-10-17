//! Simple Media Player — Stage 0: 环境验证窗口
//!
//! 目标：验证 gpui-kit 工具链可用，弹出一个 GPU 渲染的原生窗口。
//! Stage 1 将替换为播放器三栏布局。

use gpui_kit::component::{button::*, *};
use gpui_kit::*;

struct HelloWorld;

impl Render for HelloWorld {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .size_full()
            .gap_2()
            .items_center()
            .justify_center()
            .child(div().text_xl().child("Simple Media Player"))
            .child(div().text_sm().child("Stage 0: 工具链验证 ✅"))
            .child(
                Button::new("ok")
                    .primary()
                    .label("Let's Go!")
                    .on_click(cx.listener(|_this, _event, _window, cx| {
                        cx.quit();
                    })),
            )
    }
}

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);

        cx.open_window(WindowOptions::default(), |_window, cx| {
            cx.new(|_cx| HelloWorld)
        })
        .expect("failed to open window");
    });
}
