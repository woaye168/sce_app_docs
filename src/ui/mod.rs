//! 业务 UI 按标签页拆分：每个页面文件里 `impl App` 定义对应渲染函数，
//! main.rs 的 `ShellApp::ui_tab` 只做分发。
//! 新增页面 = 本目录加文件 + 此处 mod 声明 + main.rs 的 TABS / ui_tab 分发各加一行。
mod main_page;
mod settings;

#[allow(unused_imports)]
pub(crate) use settings::*;
