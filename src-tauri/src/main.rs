// 桌面端入口。发行版里不弹控制台窗口。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kanami_lib::run()
}
