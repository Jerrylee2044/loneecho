//! 右侧内容区路由与共享列表组件。

use super::*;
use crate::app::SAVE_TEXT;
use crate::ui::i18n::Language;
use gpui::MouseMoveEvent;
#[cfg(target_os = "linux")]
use sodam_core::config::TitlebarMode;
use sodam_core::EqGains;
use std::sync::Arc;

/// 字节 → 人类可读（KB / MB / GB）。
fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let value = bytes as f64;
    if value >= KB * KB * KB {
        format!("{:.2} GB", value / (KB * KB * KB))
    } else if value >= KB * KB {
        format!("{:.1} MB", value / (KB * KB))
    } else if value >= KB {
        format!("{:.0} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

/// 秒 → 「小时:分:秒 / 分:秒」。
pub(crate) fn duration_of(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (hours, minutes, secs) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

/// 音效滑杆轨道宽度与滑块直径（设置页用）。
const EQ_TRACK_W: f32 = 150.0;
const EQ_THUMB: f32 = 11.0;
/// 滑杆槽位约定：0..10 = EQ 段（dB），10 = 前级（dB），11/12 = 混响强度/空间大小（%）。
const PREAMP_SLOT: usize = 10;
const REVERB_WET_SLOT: usize = 11;
const REVERB_ROOM_SLOT: usize = 12;

/// 音效页的三个卡片：预设（开关 + 12 预设）、均衡器（前级 + 10 段）、空间混响。
/// 卡片标题与页面标题由页面层提供，这里只产出卡片内容。
fn effects_presets_card(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let eq = root.settings.eq;
    let toggle_label = if eq.enabled {
        root.tr("关闭音效")
    } else {
        root.tr("开启音效")
    };
    let preset_pills = EqGains::PRESETS
        .iter()
        .enumerate()
        .map(|(index, (key, preset))| {
            let label = root.tr(key);
            let chosen = eq == *preset;
            div()
                .id(("eq-preset", index))
                .h(px(theme::size::CONTROL_SM))
                .px(px(theme::space::MD))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme::radius::ROW))
                .bg(theme::surface_elevated())
                .hover(|style| style.bg(theme::surface_hover()))
                .cursor_pointer()
                .text_size(theme::Text::Small.size())
                .text_color(if chosen {
                    theme::accent()
                } else {
                    theme::text()
                })
                .child(label)
                .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                    root.set_eq(*preset, cx);
                }))
        })
        .collect::<Vec<_>>();

    let toggle = div()
        .id("eq-toggle")
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(theme::size::CONTROL_SM))
        .px(px(theme::space::MD))
        .rounded(px(theme::radius::ROW))
        .bg(if eq.enabled {
            theme::accent()
        } else {
            theme::surface_elevated()
        })
        .text_color(if eq.enabled {
            theme::accent_foreground()
        } else {
            theme::text()
        })
        .cursor_pointer()
        .hover(|style| style.opacity(0.9))
        .text_size(theme::Text::Small.size())
        .child(toggle_label)
        .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
            let next = EqGains {
                enabled: !root.settings.eq.enabled,
                ..root.settings.eq
            };
            root.set_eq(next, cx);
        }));

    settings_card(Some("音效预设"))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(theme::space::SM))
                .child(toggle)
                .children(preset_pills),
        )
        .into_any_element()
}

/// 均衡器卡片内容：前级 + 10 段频段，flex_wrap 自适应网格。
/// 每个滑杆行自然宽 262px：内容区宽时两列（甚至更多）并排，
/// 窗口窄时自动折成单列，绝不溢出卡片。
fn effects_eq_card(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let cells: Vec<AnyElement> = std::iter::once(PREAMP_SLOT)
        .chain(0..sodam_core::eq::BAND_COUNT)
        .map(|slot| {
            div()
                .flex_none()
                .child(eq_slider_row(root, slot, cx))
                .into_any_element()
        })
        .collect();
    settings_card(Some("均衡器"))
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(theme::space::MD))
                .children(cells),
        )
        .into_any_element()
}

/// 空间混响卡片内容：强度 + 空间大小（宽屏并排，窄屏折行）。
fn effects_reverb_card(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    settings_card(Some("空间混响"))
        .child(
            div()
                .text_size(theme::Text::Tiny.size())
                .text_color(theme::text_faint())
                .child(root.tr("混响强度为 0 时无混响；现场/古典等预设自带混响")),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(theme::space::MD))
                .child(
                    div()
                        .flex_none()
                        .child(eq_slider_row(root, REVERB_WET_SLOT, cx)),
                )
                .child(
                    div()
                        .flex_none()
                        .child(eq_slider_row(root, REVERB_ROOM_SLOT, cx)),
                ),
        )
        .into_any_element()
}

/// 一行滑杆：标签（52px）+ 轨道 + 当前值（64px）。
fn eq_slider_row(root: &Root, slot: usize, cx: &mut Context<Root>) -> AnyElement {
    let (label, value_text) = match slot {
        PREAMP_SLOT => (
            root.tr("前级"),
            format!("{:+.0} dB", root.settings.eq.preamp_db),
        ),
        REVERB_WET_SLOT => (
            root.tr("混响强度"),
            format!("{:.0}%", root.settings.eq.reverb_wet * 100.0),
        ),
        REVERB_ROOM_SLOT => (
            root.tr("空间大小"),
            format!("{:.0}%", root.settings.eq.reverb_room * 100.0),
        ),
        band => (
            sodam_core::eq::BAND_LABELS[band],
            format!("{:+.0} dB", root.settings.eq.bands[band]),
        ),
    };
    div()
        .flex()
        .items_center()
        .gap(px(theme::space::SM))
        .child(
            div()
                .w(px(44.0))
                .flex_none()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text())
                .child(label),
        )
        .child(eq_slider(root, slot, cx))
        .child(
            div()
                .w(px(52.0))
                .flex_none()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(value_text),
        )
        .into_any_element()
}

/// 单个音效滑杆（dB 或百分比两类，槽位约定见 eq_section）。
fn eq_slider(root: &Root, slot: usize, cx: &mut Context<Root>) -> impl IntoElement {
    let range = sodam_core::eq::EQ_GAIN_RANGE_DB;
    let ratio = match slot {
        PREAMP_SLOT => ((root.settings.eq.preamp_db + range) / (2.0 * range)).clamp(0.0, 1.0),
        REVERB_WET_SLOT => root.settings.eq.reverb_wet.clamp(0.0, 1.0),
        REVERB_ROOM_SLOT => root.settings.eq.reverb_room.clamp(0.0, 1.0),
        band => ((root.settings.eq.bands[band] + range) / (2.0 * range)).clamp(0.0, 1.0),
    };
    let filled = (EQ_TRACK_W - EQ_THUMB) * ratio;
    let bounds_slot = root.eq_track_bounds[slot].clone();
    let apply = move |x: f32, root: &mut Root, cx: &mut Context<Root>| {
        let left = bounds_slot
            .lock()
            .ok()
            .and_then(|value| value.map(|rect| f32::from(rect.left())))
            .unwrap_or(0.0);
        let ratio = ((x - left) / EQ_TRACK_W).clamp(0.0, 1.0);
        let mut next = root.settings.eq;
        match slot {
            PREAMP_SLOT => next.preamp_db = (ratio * 2.0 * range - range).round(),
            REVERB_WET_SLOT => next.reverb_wet = (ratio * 20.0).round() / 20.0,
            REVERB_ROOM_SLOT => next.reverb_room = (ratio * 20.0).round() / 20.0,
            band => next.bands[band] = (ratio * 2.0 * range - range).round(),
        }
        root.set_eq(next, cx);
    };
    let apply_down = apply.clone();
    let apply_move = apply.clone();
    let bounds_for_canvas = root.eq_track_bounds[slot].clone();

    div()
        .relative()
        .flex()
        .items_center()
        .h(px(16.0))
        .w(px(EQ_TRACK_W))
        .flex_none()
        .child(
            div()
                .absolute()
                .top(px(6.5))
                .left(px(0.0))
                .w(px(EQ_TRACK_W))
                .h(px(3.0))
                .rounded(px(2.0))
                .bg(theme::surface_hover()),
        )
        .child(
            div()
                .absolute()
                .top(px(6.5))
                .left(px(0.0))
                .w(px(filled + EQ_THUMB / 2.0))
                .h(px(3.0))
                .rounded(px(2.0))
                .bg(theme::accent()),
        )
        .child(
            div()
                .absolute()
                .top(px((16.0 - EQ_THUMB) / 2.0))
                .left(px(filled))
                .size(px(EQ_THUMB))
                .rounded_full()
                .bg(theme::text()),
        )
        .child(
            canvas(
                move |bounds: Bounds<gpui::Pixels>, _window, _cx| {
                    if let Ok(mut guard) = bounds_for_canvas.lock() {
                        *guard = Some(bounds);
                    }
                },
                |_bounds, _state, _window, _cx| {},
            )
            .absolute()
            .size_full(),
        )
        .child(
            div()
                .id(("eq-track", slot))
                .absolute()
                .size_full()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |root, event: &MouseDownEvent, _window, cx| {
                        apply_down(f32::from(event.position.x), root, cx);
                    }),
                )
                .on_mouse_move(
                    cx.listener(move |root, event: &MouseMoveEvent, _window, cx| {
                        if event.pressed_button == Some(MouseButton::Left) {
                            apply_move(f32::from(event.position.x), root, cx);
                        }
                    }),
                ),
        )
}

/// 设置页：音质偏好选择 + 账号信息 + 其他。
/// 设置页 → 账户板块：头像 + 昵称/ID/VIP；未登录给「去登录」并弹二维码 modal。
/// 账号信息卡：头像 + 昵称/ID/VIP + 退出登录（未登录给「去登录」）。
/// 设置页的页头由 views/mod.rs 框架层渲染（与「设置」标题同一行）。
pub(crate) fn account_section(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let logged_in = !root.settings.cookie.trim().is_empty();
    let avatar_path = root
        .account
        .as_ref()
        .filter(|_| logged_in)
        .map(|info| info.avatar_url.clone())
        .filter(|url| !url.trim().is_empty())
        .and_then(|url| root.cover_of(&url));

    // 通栏信息条：不带卡片外壳（无圆角/背景/描边），宽度由调用方撑满，
    // 与设置页头同一行、与下方内容区左右对齐。
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::space::LG))
        .py(px(theme::space::SM))
        .child(if logged_in {
            cover(avatar_path, 48.0, 999.0)
        } else {
            cover(None, 48.0, 999.0)
        })
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(theme::space::XS))
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(if logged_in {
                            match &root.account {
                                Some(info) if !info.nickname.is_empty() => info.nickname.clone(),
                                _ => root.tr("已登录").to_string(),
                            }
                        } else {
                            root.tr("未登录").to_string()
                        }),
                )
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(if logged_in {
                            match &root.account {
                                Some(info) => {
                                    let vip = if info.vip {
                                        root.tr("VIP")
                                    } else {
                                        root.tr("非 VIP")
                                    };
                                    root.localized(
                                        "用户 {} · {}",
                                        &[info.user_id.to_string(), vip.to_string()],
                                    )
                                }
                                None => root.tr("正在读取账号信息…").to_string(),
                            }
                        } else {
                            root.tr("登录后才能使用搜索、歌单与播放").to_string()
                        }),
                )
                .when(!logged_in, |this| {
                    this.child(
                        div()
                            .id("go-login")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(theme::space::SM))
                            .h(px(theme::size::CONTROL))
                            .px(px(theme::space::LG))
                            .mt(px(theme::space::SM))
                            .rounded(px(theme::radius::PILL))
                            .bg(theme::accent())
                            .cursor_pointer()
                            .hover(|style| style.opacity(0.9))
                            .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                root.login_modal_open = true;
                                root.start_login(cx);
                            }))
                            .child(
                                svg()
                                    .path(icons::path("log-in"))
                                    .size(px(theme::ICON))
                                    .text_color(theme::accent_foreground()),
                            )
                            .child(
                                div()
                                    .text_size(theme::Text::Small.size())
                                    .text_color(theme::accent_foreground())
                                    .child(root.tr("去登录")),
                            ),
                    )
                }),
        )
        .when(logged_in, |this| {
            this.child(
                div()
                    .id("logout")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::space::SM))
                    .flex_none()
                    .h(px(theme::size::CONTROL))
                    .px(px(theme::space::LG))
                    .rounded(px(theme::radius::PILL))
                    .bg(theme::surface_hover())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_selected()))
                    .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                        root.logout(cx);
                    }))
                    .child(
                        svg()
                            .path(icons::path("x"))
                            .size(px(theme::ICON))
                            .text_color(theme::text_muted()),
                    )
                    .child(
                        div()
                            .text_size(theme::Text::Small.size())
                            .text_color(theme::text_muted())
                            .child(root.tr("退出登录")),
                    ),
            )
        })
        .into_any_element()
}

/// Linux 专属：窗口控制按钮的显示策略。
///
/// 只有 Linux 需要这个开关：Windows 一定会去掉系统标题栏、macOS 用系统红绿灯，
/// 其它平台直接返回空，不占布局。
#[cfg(target_os = "linux")]
fn window_controls_section(root: &Root, cx: &mut Context<Root>) -> Option<AnyElement> {
    let current = TitlebarMode::from_key(&root.settings.titlebar);
    let rows: Vec<AnyElement> = [
        (
            TitlebarMode::Auto,
            root.tr("自动（跟随桌面）"),
            root.tr("由桌面是否要求客户端自绘决定：GNOME 等显示，Hyprland 等平铺桌面不显示"),
        ),
        (
            TitlebarMode::Always,
            root.tr("总是显示"),
            root.tr("即使桌面自己不管窗口，也显示最小化 / 最大化 / 关闭按钮"),
        ),
        (
            TitlebarMode::Never,
            root.tr("不显示"),
            root.tr("完全交给桌面管理窗口，不画任何按钮"),
        ),
    ]
    .into_iter()
    .map(|(mode, title, hint)| {
        let chosen = mode == current;
        div()
            .id(gpui::ElementId::Name(
                format!("titlebar-set-{}", mode.key()).into(),
            ))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(theme::space::LG))
            .px(px(theme::space::MD))
            .py(px(theme::space::SM))
            .rounded(px(theme::radius::ROW))
            .cursor_pointer()
            .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                root.settings.titlebar = mode.key().to_string();
                if let Err(err) = root.settings.save() {
                    let message = root.failure_text("窗口按钮已切换，但保存失败", &err, SAVE_TEXT);
                    root.notify_error_once("titlebar-save-failed", message);
                } else {
                    let status = match mode {
                        TitlebarMode::Auto => root.tr("窗口按钮：自动（跟随桌面）").to_string(),
                        TitlebarMode::Always => root.tr("窗口按钮：总是显示").to_string(),
                        TitlebarMode::Never => root.tr("窗口按钮：不显示").to_string(),
                    };
                    root.set_status_text(status);
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(theme::Text::Body.size())
                            .text_color(if chosen {
                                theme::text()
                            } else {
                                theme::text_muted()
                            })
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(theme::Text::Small.size())
                            .text_color(theme::text_muted())
                            .child(hint),
                    ),
            )
            .when(chosen, |this| {
                this.child(
                    svg()
                        .path(icons::path("check"))
                        .size(px(theme::ICON))
                        .text_color(theme::accent()),
                )
            })
            .into_any_element()
    })
    .collect();

    Some(
        div()
            .flex()
            .flex_col()
            .gap(px(theme::space::XS))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(theme::space::XS))
                    .children(rows),
            )
            .into_any_element(),
    )
}

#[cfg(not(target_os = "linux"))]
fn window_controls_section(_root: &Root, _cx: &mut Context<Root>) -> Option<AnyElement> {
    None
}

fn search_history_section(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let enabled = root.settings.search_history_is_enabled();
    let count = root.settings.search_history.len();
    let mut section = div().flex().flex_col().gap(px(theme::space::XS)).child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(theme::space::LG))
            .px(px(theme::space::MD))
            .py(px(theme::space::SM))
            .child(
                div()
                    .text_size(theme::Text::Body.size())
                    .text_color(theme::text())
                    .child(root.tr("保存搜索历史")),
            )
            .child(crate::ui::switch::render(
                "search-history-toggle",
                enabled,
                |root, _window, cx| root.toggle_search_history(cx),
                cx,
            )),
    );

    if count > 0 {
        section = section.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(theme::space::MD))
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.localized("已保存 {} 条搜索历史", &[count.to_string()])),
                )
                .child(
                    div()
                        .id("settings-search-history-clear")
                        .cursor_pointer()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::accent())
                        .hover(|style| style.text_color(theme::text()))
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.clear_search_history(cx);
                        }))
                        .child(root.tr("清空")),
                ),
        );
    }

    section.into_any_element()
}

/// 设置页 → 诊断区块的一行开关：左侧标题与说明，右侧是既有的 switch 组件。
fn log_toggle_row(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(theme::space::LG))
        .px(px(theme::space::MD))
        .py(px(theme::space::SM))
        .rounded(px(theme::radius::ROW))
        .bg(theme::surface())
        .child(
            div()
                .flex()
                .flex_col()
                .min_w(px(0.0))
                .flex_1()
                .child(
                    div()
                        .text_size(theme::Text::Body.size())
                        .text_color(theme::text())
                        .child(root.tr("详细日志")),
                )
                .child(
                    div()
                        .text_size(theme::Text::Tiny.size())
                        .text_color(theme::text_muted())
                        .child(
                            root.tr(
                                "多记播放、预取与界面耗时等调试信息；仍不含 Cookie、签名与查询串",
                            ),
                        ),
                ),
        )
        .child(crate::ui::switch::render(
            "detailed-logs-toggle",
            root.settings.detailed_logs,
            |root, _window, cx| {
                let enabled = !root.settings.detailed_logs;
                root.set_detailed_logs(enabled, cx);
            },
            cx,
        ))
        .into_any_element()
}

/// 与「缓存」刻意分开：日志在数据目录，`清空缓存` 不碰它（见 #53）。
fn diagnostic_log_section(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let log_dir = sodam_core::log_outlet::log_dir();
    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::SM))
        .child(
            div()
                .truncate()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(root.localized("日志目录：{}", &[log_dir.display().to_string()])),
        )
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_faint())
                .child(root.tr("日志可能含接口地址（查询串已移除），请勿公开上传")),
        )
        .child(log_toggle_row(root, cx))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(theme::space::SM))
                .children([
                    div()
                        .id("open-log-directory")
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(theme::size::CONTROL))
                        .w(px(140.0))
                        .rounded(px(theme::radius::ROW))
                        .bg(theme::surface_elevated())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text())
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.open_log_directory(cx);
                        }))
                        .child(root.tr("打开日志目录"))
                        .into_any_element(),
                    div()
                        .id("clear-diagnostic-logs")
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(theme::size::CONTROL))
                        .w(px(140.0))
                        .rounded(px(theme::radius::ROW))
                        .bg(theme::surface_elevated())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text())
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.clear_diagnostic_logs(cx);
                        }))
                        .child(if root.log_clearing {
                            root.tr("正在清除…")
                        } else {
                            root.tr("清除诊断日志")
                        })
                        .into_any_element(),
                ]),
        )
        .into_any_element()
}

/// 设置页的三级分类（中栏二级菜单项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsSection {
    #[default]
    Theme,
    Language,
    SearchHistory,
    Window,
    Quality,
    Effects,
    Cache,
    Shortcuts,
    About,
}

impl SettingsSection {
    /// 菜单展示顺序。窗口控制只对 Linux 有意义（macOS/Windows 由系统负责装饰）。
    fn all() -> &'static [SettingsSection] {
        &[
            Self::Theme,
            Self::Language,
            Self::SearchHistory,
            #[cfg(target_os = "linux")]
            Self::Window,
            Self::Quality,
            Self::Effects,
            Self::Cache,
            Self::Shortcuts,
            Self::About,
        ]
    }

    fn id(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Language => "language",
            Self::SearchHistory => "search-history",
            Self::Window => "window",
            Self::Quality => "quality",
            Self::Effects => "effects",
            Self::Cache => "cache",
            Self::Shortcuts => "shortcuts",
            Self::About => "about",
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::Theme => "主题",
            Self::Language => "语言",
            Self::SearchHistory => "搜索历史",
            Self::Window => "窗口",
            Self::Quality => "音质偏好",
            Self::Effects => "音效",
            Self::Cache => "缓存",
            Self::Shortcuts => "快捷键",
            Self::About => "关于",
        }
    }
}

/// 三级页里的分组卡片：圆角描边容器，可选内标题（参考官方客户端的分组卡）。
fn settings_card(title_key: Option<&'static str>) -> gpui::Div {
    // 纯布局分组: 无卡片外壳 (无圆角/背景/描边), 内容直接平铺在页面上。
    let mut card = div()
        .flex()
        .flex_col()
        .gap(px(theme::space::SM))
        .py(px(theme::space::MD));
    if let Some(key) = title_key {
        card = card.child(
            div()
                .text_size(theme::Text::Small.size())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(key),
        );
    }
    card
}

/// 三级页页头：大标题 + 灰色副标题。
fn page_header(title: &'static str, subtitle: Option<String>) -> AnyElement {
    let mut header = div().flex().flex_col().gap(px(theme::space::XS)).child(
        div()
            .text_size(theme::Text::Large.size())
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme::text())
            .child(title),
    );
    if let Some(subtitle) = subtitle {
        header = header.child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(subtitle),
        );
    }
    header.into_any_element()
}

/// 设置入口：左中栏（设置卡片式菜单）+ 右栏（三级配置页，独立滚动）。
pub(crate) fn settings_view(root: &mut Root, cx: &mut Context<Root>) -> AnyElement {
    if root.settings_section == SettingsSection::About {
        root.ensure_contributors(cx);
    }
    // 无「设置」页头：账号条常驻右栏顶部（通栏），其下是三级配置页（独立滚动）。
    div()
        .flex()
        .flex_row()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::space::LG))
        .child(settings_menu(root, cx))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .min_h(px(0.0))
                .gap(px(theme::space::LG))
                .child(account_section(root, cx))
                .child(
                    div()
                        .id("settings-scroll")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_y_scroll()
                        .child(settings_page(root, cx)),
                ),
        )
        .into_any_element()
}

/// 中栏：二级分类菜单。纯文字列表——无卡片外壳、无选中高亮块，
/// 选中项只用强调色文字表示；菜单文字与上方「设置」标题左对齐。
fn settings_menu(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    div()
        .w(px(170.0))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(theme::space::XS))
        .children(SettingsSection::all().iter().map(|section| {
            let selected = *section == root.settings_section;
            div()
                .id(gpui::ElementId::Name(
                    format!("settings-menu-{}", section.id()).into(),
                ))
                .mx(px(theme::space::SM))
                .px(px(theme::space::MD))
                .py(px(theme::space::SM))
                .cursor_pointer()
                .rounded(px(theme::radius::ROW))
                .text_size(theme::Text::Body.size())
                .text_color(if selected {
                    theme::accent()
                } else {
                    theme::text()
                })
                .hover(|style| style.bg(theme::surface_hover()))
                .child(root.tr(section.label_key()))
                .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                    root.set_settings_section(*section, cx);
                }))
        }))
        .into_any_element()
}

/// 右栏：按选中的二级菜单项渲染三级配置页。
fn settings_page(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    match root.settings_section {
        SettingsSection::Theme => theme_page(root, cx),
        SettingsSection::Language => language_page(root, cx),
        SettingsSection::SearchHistory => search_history_page(root, cx),
        SettingsSection::Window => window_page(root, cx),
        SettingsSection::Quality => quality_page(root, cx),
        SettingsSection::Effects => effects_page(root, cx),
        SettingsSection::Cache => cache_page(root, cx),
        SettingsSection::Shortcuts => shortcuts_page(root, cx),
        SettingsSection::About => about_page(root, cx),
    }
}
/// 主题页：跟随系统 / 深色 / 浅色。
fn theme_page(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let subtitle = if root.theme_follows_system {
        root.tr("当前跟随系统偏好，手动选择后会固定主题")
    } else {
        root.tr("已固定主题；可随时切换深色或浅色")
    };
    let theme_rows: Vec<AnyElement> = [
        (None, root.tr("跟随系统")),
        (Some(ThemeKind::Dark), root.tr("深色")),
        (Some(ThemeKind::Light), root.tr("浅色")),
    ]
    .into_iter()
    .map(|(value, title)| {
        // `None` = 跟随系统；`Some(kind)` = 固定主题。
        let chosen = match value {
            None => root.theme_follows_system,
            Some(kind) => !root.theme_follows_system && root.theme == kind,
        };
        let id = match value {
            None => "theme-set-system".to_string(),
            Some(kind) => format!("theme-set-{}", kind.key()),
        };
        // 跟随系统的色块用黑白对角渐变，直观表示「深色/浅色都由系统定」。
        let swatch: gpui::Background = match value {
            None => gpui::linear_gradient(
                135.0,
                gpui::linear_color_stop(theme::hex(0x0A0A0A), 0.0),
                gpui::linear_color_stop(theme::hex(0xFFFFFF), 1.0),
            ),
            Some(ThemeKind::Light) => theme::hex(0xFFFFFF).into(),
            Some(ThemeKind::Dark) => theme::hex(0x0A0A0A).into(),
        };
        div()
            .id(gpui::ElementId::Name(id.into()))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(theme::space::LG))
            .px(px(theme::space::MD))
            .py(px(theme::space::SM))
            .rounded(px(theme::radius::ROW))
            .cursor_pointer()
            .on_click(
                cx.listener(move |root, _event: &ClickEvent, _window, cx| match value {
                    None => root.set_ui_theme_system(cx),
                    Some(kind) => root.set_ui_theme(kind, cx),
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::space::SM))
                    .child(
                        div()
                            .size(px(14.0))
                            .rounded(px(theme::radius::PILL))
                            .border_1()
                            .border_color(theme::border())
                            .bg(swatch),
                    )
                    .child(
                        div()
                            .text_size(theme::Text::Body.size())
                            .text_color(if chosen {
                                theme::text()
                            } else {
                                theme::text_muted()
                            })
                            .child(title),
                    ),
            )
            .when(chosen, |this| {
                this.child(
                    svg()
                        .path(icons::path("check"))
                        .size(px(theme::ICON))
                        .text_color(theme::accent()),
                )
            })
            .into_any_element()
    })
    .collect();

    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::LG))
        .child(page_header(root.tr("主题"), Some(subtitle.to_string())))
        .child(settings_card(None).children(theme_rows))
        .into_any_element()
}

/// 语言页：跟随系统 / 中文 / English。
fn language_page(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let subtitle = if root.language_follows_system {
        root.tr("当前跟随系统语言；手动选择后会固定语言")
    } else {
        root.tr("已固定语言；可随时切换中文或 English")
    };
    let language_rows: Vec<AnyElement> = [
        (Language::System, root.tr("跟随系统")),
        (Language::Chinese, "中文"),
        (Language::English, "English"),
    ]
    .into_iter()
    .map(|(value, title)| {
        let chosen = if root.language_follows_system {
            matches!(value, Language::System)
        } else {
            !matches!(value, Language::System) && root.language == value
        };
        div()
            .id(gpui::ElementId::Name(
                format!("language-set-{:?}", value).into(),
            ))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(theme::space::LG))
            .px(px(theme::space::MD))
            .py(px(theme::space::SM))
            .rounded(px(theme::radius::ROW))
            .cursor_pointer()
            .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                match value {
                    Language::System => {
                        root.language = Language::system_locale();
                        root.language_follows_system = true;
                        root.settings.language = "auto".to_string();
                        if let Err(err) = root.settings.save() {
                            let message = root.failure_text(
                                "语言已恢复为跟随系统，但保存失败",
                                &err,
                                SAVE_TEXT,
                            );
                            root.notify_error_once("language-save-failed", message);
                        } else {
                            let status = if root.language.is_zh() {
                                root.tr("语言已恢复为跟随系统").to_string()
                            } else {
                                "Language restored to system".to_string()
                            };
                            root.set_status_text(status);
                        }
                        // macOS 菜单栏的标题也要跟着换语言。
                        #[cfg(target_os = "macos")]
                        crate::menus::refresh(cx, root.language);
                    }
                    language => root.set_language(language, cx),
                }
                cx.notify();
            }))
            .child(
                div()
                    .text_size(theme::Text::Body.size())
                    .text_color(if chosen {
                        theme::text()
                    } else {
                        theme::text_muted()
                    })
                    .child(title),
            )
            .when(chosen, |this| {
                this.child(
                    svg()
                        .path(icons::path("check"))
                        .size(px(theme::ICON))
                        .text_color(theme::accent()),
                )
            })
            .into_any_element()
    })
    .collect();

    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::LG))
        .child(page_header(root.tr("语言"), Some(subtitle.to_string())))
        .child(settings_card(None).children(language_rows))
        .into_any_element()
}

/// 搜索历史页。
fn search_history_page(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::LG))
        .child(page_header(
            root.tr("搜索历史"),
            Some(
                root.tr("仅保存在本机，最多保留 10 条；关闭后会清空记录")
                    .to_string(),
            ),
        ))
        .child(settings_card(None).child(search_history_section(root, cx)))
        .into_any_element()
}

/// 窗口页（仅 Linux 菜单可见）：是否自绘标题栏按钮。
fn window_page(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::LG))
        .child(page_header(
            root.tr("窗口"),
            Some(
                root.tr("Linux 下是否自绘最小化 / 最大化 / 关闭按钮")
                    .to_string(),
            ),
        ))
        .child(settings_card(None).children(window_controls_section(root, cx)))
        .into_any_element()
}

/// 音质页：意图选择 + 来源徽章。
fn quality_page(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    // 档位与名称都从核心层派生（`TIERS` + `label_key`），这里只保留每档的说明文案：
    // 否则往 `TIERS` 加一档时，设置页会静默少一项。
    // 档位是“上限”而不是“目标”：libresoda 在没有任何候选低于上限时，会回退成全部候选
    //（实际可能比所选更高），所以这里说“优先 / 由服务端回退”，不承诺“只降一档”。
    let hint_key = |value: &str| -> &'static str {
        match value {
            "hires" => "优先 Hi-Res 无损；没有可用档位时由服务端回退",
            "lossless" => "优先无损；没有可用无损时由服务端回退",
            "dolby" => "优先杜比全景声；没有可用档位时由服务端回退",
            "highest" => "优先极高（≈320k）；没有可用档位时由服务端回退",
            "medium" => "优先较高；没有可用档位时由服务端回退",
            "low" => "优先流畅（≈128k）；没有可用档位时由服务端回退",
            _ => "不设上限，按账号权益和单曲可用档位取最高",
        }
    };
    let language = root.language;
    let label_for = |value: &str| crate::ui::player_bar::quality_label(value, language);
    let options: Vec<(&str, String)> = std::iter::once(sodam_core::quality::AUTO)
        .chain(sodam_core::quality::TIERS)
        .map(|value| (value, label_for(value)))
        .collect();
    // 勾选的是**意图**；配置里的空串是历史写法，按「自动」显示。
    let current = root.settings.quality_intent().as_setting().to_string();
    // 来源提示与徽章：区分用户手动选择、程序推导与未知来源。
    // 在全新未配置的「自动」项上（Unknown + Auto）不展示徽章，避免新用户误解。
    let (source_badge, source_hint) =
        match (root.settings.quality_source, root.settings.quality_intent()) {
            (sodam_core::quality::QualitySource::Derived, _) => (
                Some(root.tr("程序推导")),
                Some(root.tr("当前档位由程序推导，点「自动」即可恢复跟随账号权益")),
            ),
            (
                sodam_core::quality::QualitySource::Unknown,
                sodam_core::quality::QualityIntent::Auto,
            ) => (None, None),
            (sodam_core::quality::QualitySource::Unknown, _) => (
                Some(root.tr("来源未知")),
                Some(root.tr("若不是你选的，点「自动」即可恢复跟随账号权益")),
            ),
            (sodam_core::quality::QualitySource::User, _) => (
                Some(root.tr("手动选择")),
                Some(root.tr("当前档位由手动选择")),
            ),
        };

    let rows: Vec<AnyElement> = options
        .iter()
        .map(|(value, title)| {
            let chosen = *value == current;
            let hint = root.tr(hint_key(value));
            let value = value.to_string();
            div()
                .id(gpui::ElementId::Name(format!("quality-set-{value}").into()))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(theme::space::LG))
                .px(px(theme::space::MD))
                .py(px(theme::space::SM))
                .rounded(px(theme::radius::ROW))
                .cursor_pointer()
                .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                    root.set_quality(&value, cx);
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(theme::space::SM))
                                .child(
                                    div()
                                        .text_size(theme::Text::Body.size())
                                        .text_color(if chosen {
                                            theme::text()
                                        } else {
                                            theme::text_muted()
                                        })
                                        .child(title.clone()),
                                )
                                .when(chosen, |this| {
                                    this.when_some(source_badge, |this, badge| {
                                        this.child(
                                            div()
                                                .text_size(theme::Text::Tiny.size())
                                                .text_color(theme::text_muted())
                                                .px(px(6.0))
                                                .py(px(1.0))
                                                .rounded(px(theme::radius::PILL))
                                                .bg(theme::surface_hover())
                                                .child(badge),
                                        )
                                    })
                                }),
                        )
                        .child(
                            div()
                                .text_size(theme::Text::Tiny.size())
                                .text_color(theme::text_faint())
                                .child(hint),
                        ),
                )
                .when(chosen, |this| {
                    this.child(
                        svg()
                            .path(icons::path("check"))
                            .size(px(theme::ICON))
                            .text_color(theme::accent()),
                    )
                })
                .into_any_element()
        })
        .collect();

    let mut card = settings_card(None).children(rows);
    if let Some(hint) = source_hint {
        card = card.child(
            div()
                .text_size(theme::Text::Tiny.size())
                .text_color(theme::text_faint())
                .child(hint),
        );
    }

    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::LG))
        .child(page_header(
            root.tr("音质偏好"),
            Some(
                root.tr("自动 = 取当前账号能拿到的最高档，单曲没有该档位时自动降一档")
                    .to_string(),
            ),
        ))
        .child(card)
        .into_any_element()
}

/// 音效页：预设 + 均衡器 + 空间混响，三个分组卡片。
fn effects_page(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::LG))
        .child(page_header(
            root.tr("音效"),
            Some(
                root.tr("正值增强、负值减弱；立即生效并随配置保存")
                    .to_string(),
            ),
        ))
        .child(effects_presets_card(root, cx))
        .child(effects_eq_card(root, cx))
        .child(effects_reverb_card(root, cx))
        .into_any_element()
}

/// 缓存页：占用统计 / 上限 / 清理 + 批量升级缓存。
fn cache_page(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    // 显示占用大小 + 一键清理（统计读后台快照，不在渲染路径扫盘）
    let (audio_bytes, audio_files, cover_bytes, cover_files) = root.cache_summary;
    let total = audio_bytes + cover_bytes;
    let usage = if root.language.resolved().is_zh() {
        root.localized(
            "共 {}（歌曲 {} 首 {} / 封面 {} 张 {}）",
            &[
                human_bytes(total),
                audio_files.to_string(),
                human_bytes(audio_bytes),
                cover_files.to_string(),
                human_bytes(cover_bytes),
            ],
        )
    } else {
        format!(
            "Total {} ({} songs {}, {} covers {})",
            human_bytes(total),
            audio_files,
            human_bytes(audio_bytes),
            cover_files,
            human_bytes(cover_bytes)
        )
    };

    // 批量升级同样从 `TIERS` 派生，避免播放档位能选、批量升级却少几档。
    let language = root.language;
    let label_for = |value: &str| crate::ui::player_bar::quality_label(value, language);
    let batch_tiers: Vec<(&str, String)> = sodam_core::quality::TIERS
        .into_iter()
        .map(|tier| (tier, label_for(tier)))
        .collect();
    let batch_tier_rows: Vec<AnyElement> = batch_tiers
        .into_iter()
        .enumerate()
        .map(|(index, (tier, label))| {
            let is_selected = root.batch_upgrade_target
                == sodam_core::quality::QualityIntent::Tier(tier.to_string());
            div()
                .id(("batch-upgrade-tier", index))
                .h(px(theme::size::CONTROL_SM))
                .px(px(theme::space::MD))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme::radius::ROW))
                .bg(theme::surface_elevated())
                .hover(|style| style.bg(theme::surface_hover()))
                .cursor_pointer()
                .text_size(theme::Text::Small.size())
                .text_color(if is_selected {
                    theme::accent()
                } else {
                    theme::text()
                })
                .child(label)
                .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                    if !root.batch_upgrading {
                        root.set_batch_upgrade_target(
                            sodam_core::quality::QualityIntent::Tier(tier.to_string()),
                            cx,
                        );
                    }
                }))
                .into_any_element()
        })
        .collect();
    let batch_target = root.batch_upgrade_target.clone();
    let batch_target_label = label_for(batch_target.as_setting());
    // 需要会员的档位只在账号**确认**有权益时才放行；与 `start_batch_upgrade` 共用同一条
    // 判定，按钮状态和执行结果不会各说各话。
    let batch_upgrade_blocked_by_vip =
        sodam_core::quality::batch_upgrade_needs_confirmed_entitlement(
            &batch_target,
            root.account.as_ref().map(|account| account.vip),
        );

    let estimate = {
        // 档位未知的旧缓存另起一行（#89）：这几首不是「已知比目标低」，是旧版本
        // 没记档位。被权益挡住、或这一轮全都抑制掉了，也要照样报出来——
        // 「没有可升级的缓存歌曲」在这种存量下就不是实话（P5 的原话）。
        // 只报事实、不写「需重下」：这一轮到底会不会重下已经由上一行的可升级数说过。
        let legacy_note = if root.batch_upgrading {
            None
        } else {
            root.batch_upgrade_estimate
                .as_ref()
                .filter(|estimate| estimate.unknown_rank_count > 0)
                .map(|estimate| {
                    root.localized(
                        "{} 首旧缓存未记录档位",
                        &[estimate.unknown_rank_count.to_string()],
                    )
                })
        };
        let estimate_text = if batch_upgrade_blocked_by_vip {
            root.localized(
                "「{}」音质需 VIP 会员权限，当前账号未确认该权益",
                std::slice::from_ref(&batch_target_label),
            )
        } else if root.batch_upgrading {
            root.localized(
                "正在升级 ({} / {})…",
                &[
                    root.batch_upgrade_progress.done.to_string(),
                    root.batch_upgrade_progress.total.to_string(),
                ],
            )
        } else if let Some(estimate) = &root.batch_upgrade_estimate {
            if estimate.upgradeable_count == 0 {
                root.tr("没有可升级的缓存歌曲：已达目标、无更高档位或当前缓存条件不支持升级")
                    .to_string()
            } else {
                let delta = estimate.net_disk_delta_bytes();
                let delta_str = if delta >= 0 {
                    format!("+{}", human_bytes(delta as u64))
                } else {
                    format!("-{}", human_bytes((-delta) as u64))
                };
                root.localized(
                    "{} 首歌曲可升级 · 预估流量 {} · 存储变化 {}",
                    &[
                        estimate.upgradeable_count.to_string(),
                        human_bytes(estimate.estimated_traffic_bytes),
                        delta_str,
                    ],
                )
            }
        } else {
            root.tr("正在计算升级预估…").to_string()
        };
        div()
            .text_size(theme::Text::Small.size())
            .text_color(theme::text_muted())
            .child(estimate_text)
            .children(legacy_note)
    };

    let upgradeable = root
        .batch_upgrade_estimate
        .as_ref()
        .map_or(0, |e| e.upgradeable_count);
    let can_upgrade = !root.batch_upgrading && !batch_upgrade_blocked_by_vip && upgradeable > 0;
    let mut upgrade_btn = div()
        .id("batch-upgrade")
        .flex()
        .items_center()
        .justify_center()
        .h(px(theme::size::CONTROL))
        .w(px(140.0))
        .rounded(px(theme::radius::ROW))
        .text_size(theme::Text::Small.size());
    if can_upgrade {
        upgrade_btn = upgrade_btn
            .bg(theme::accent())
            .text_color(theme::accent_foreground())
            .cursor_pointer()
            .hover(|style| style.opacity(0.9))
            .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                root.start_batch_upgrade(cx);
            }));
    } else {
        upgrade_btn = upgrade_btn
            .bg(theme::surface_elevated())
            .text_color(theme::text_faint());
    }
    let upgrade_btn = upgrade_btn.child(if root.batch_upgrading {
        root.tr("正在升级…")
    } else {
        root.tr("批量升级")
    });

    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::LG))
        .child(page_header(root.tr("缓存"), Some(usage)))
        .child(
            settings_card(None)
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.localized(
                            "音频缓存上限：{}",
                            &[human_bytes(
                                root.settings.audio_cache_limit_mb * 1024 * 1024,
                            )],
                        )),
                )
                .child(
                    div().flex().flex_wrap().gap(px(theme::space::SM)).children(
                        [
                            (
                                "−1 GiB".to_string(),
                                root.settings.audio_cache_limit_mb.saturating_sub(1024),
                            ),
                            ("5 GiB".to_string(), 5 * 1024),
                            ("10 GiB".to_string(), 10 * 1024),
                            ("20 GiB".to_string(), 20 * 1024),
                            ("50 GiB".to_string(), 50 * 1024),
                            (
                                "+1 GiB".to_string(),
                                root.settings.audio_cache_limit_mb.saturating_add(1024),
                            ),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(index, (label, limit))| {
                            div()
                                .id(("cache-limit", index))
                                .h(px(theme::size::CONTROL_SM))
                                .px(px(theme::space::MD))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(theme::radius::ROW))
                                .bg(theme::surface_elevated())
                                .hover(|style| style.bg(theme::surface_hover()))
                                .cursor_pointer()
                                .text_size(theme::Text::Small.size())
                                .text_color(if root.settings.audio_cache_limit_mb == limit {
                                    theme::accent()
                                } else {
                                    theme::text()
                                })
                                .child(label)
                                .on_click(cx.listener(
                                    move |root, _event: &ClickEvent, _window, cx| {
                                        root.set_cache_limit_mb(limit, cx);
                                    },
                                ))
                        }),
                    ),
                )
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.tr("自动清理较早使用的歌曲；正在播放或下载的文件暂时保留")),
                )
                .child(
                    div()
                        .id("clear-cache")
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(theme::size::CONTROL))
                        .w(px(140.0))
                        .rounded(px(theme::radius::ROW))
                        .bg(theme::surface_elevated())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text())
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.clear_cache(cx);
                        }))
                        .child(if root.cache_clearing {
                            root.tr("正在清理…")
                        } else {
                            root.tr("清空缓存")
                        }),
                ),
        )
        .child(
            settings_card(Some(root.tr("批量升级缓存")))
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.tr("将已有低档位缓存重新拉取为更高音质，失败不会损坏旧缓存")),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(theme::space::SM))
                        .children(batch_tier_rows),
                )
                .child(estimate)
                .child(upgrade_btn),
        )
        .into_any_element()
}

/// 快捷键页：应用内播放控制键位（系统媒体键同样可用）。
fn shortcuts_page(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let subtitle = if cfg!(target_os = "macos") {
        // ⌃←/⌃→ 是系统「切换桌面」的默认键，绑了也收不到。
        root.tr("播放控制：系统媒体键同样可用；⌃←/⌃→ 被系统占用（切换桌面），别绑这两个")
            .to_string()
    } else {
        root.tr("播放控制：系统媒体键同样可用；这里绑的是应用内快捷键")
            .to_string()
    };
    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::LG))
        .child(page_header(root.tr("快捷键"), Some(subtitle)))
        .child(super::shortcut_card::section(root, cx))
        .into_any_element()
}

/// 关于页：签名服务与配置文件（原「其他」块）+ 版本信息 + 贡献者 + 诊断日志。
fn about_page(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let repository = env!("CARGO_PKG_REPOSITORY");
    let contributors = root.contributors_image.clone();
    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::LG))
        .child(page_header(root.tr("关于"), None))
        .child(
            settings_card(Some(root.tr("其他")))
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(if root.language.resolved().is_zh() {
                            root.localized(
                                "签名服务：{}",
                                &[if root.settings.signer_url.trim().is_empty() {
                                    root.tr("（未配置）").to_string()
                                } else {
                                    root.settings.signer_url.trim().to_string()
                                }],
                            )
                        } else {
                            format!(
                                "Signer: {}",
                                if root.settings.signer_url.trim().is_empty() {
                                    root.tr("（未配置）")
                                } else {
                                    root.settings.signer_url.trim()
                                }
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::space::SM))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_size(theme::Text::Small.size())
                                .text_color(theme::text_muted())
                                .child(if root.language.resolved().is_zh() {
                                    root.localized(
                                        "配置：{}",
                                        &[sodam_core::Settings::config_path()
                                            .display()
                                            .to_string()],
                                    )
                                } else {
                                    format!(
                                        "Config: {}",
                                        sodam_core::Settings::config_path().display()
                                    )
                                }),
                        )
                        .child(
                            div()
                                .id("edit-config")
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_center()
                                .flex_none()
                                .h(px(theme::size::CONTROL_SM))
                                .px(px(theme::space::MD))
                                .rounded(px(theme::radius::ROW))
                                .bg(theme::surface_elevated())
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::surface_hover()))
                                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                    root.open_config_file(cx);
                                }))
                                .child(
                                    div()
                                        .text_size(theme::Text::Tiny.size())
                                        .text_color(theme::text_muted())
                                        .child(root.tr("Edit")),
                                ),
                        ),
                ),
        )
        .child(
            div()
                .id("about-footer")
                .flex()
                .flex_col()
                .gap(px(theme::space::MD))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::space::MD))
                        .child(
                            img("icons/sodam-logo.svg")
                                .size(px(36.0))
                                .flex_none()
                                .rounded(px(theme::radius::ROW))
                                .object_fit(ObjectFit::Cover),
                        )
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .min_w(px(0.0))
                                .gap(px(theme::space::XS))
                                .child(
                                    div()
                                        .text_size(theme::Text::Small.size())
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::text())
                                        .child(format!("LoneEcho v{}", env!("CARGO_PKG_VERSION"))),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(theme::Text::Tiny.size())
                                        .text_color(theme::text_muted())
                                        .child(format!("GitHub: {repository}")),
                                )
                                .child(
                                    div()
                                        .text_size(theme::Text::Tiny.size())
                                        .text_color(theme::text_faint())
                                        .child(format!(
                                            "{} · {}",
                                            root.tr("维护者"),
                                            "ZephyrCheung"
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .id("open-repository")
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .h(px(theme::size::CONTROL_SM))
                                .px(px(theme::space::MD))
                                .rounded(px(theme::radius::ROW))
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::surface_hover()))
                                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                    root.open_github_repository(cx);
                                }))
                                .child(
                                    svg()
                                        .path(icons::path("chevron-right"))
                                        .size(px(theme::ICON_SM))
                                        .text_color(theme::text_faint()),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(theme::space::SM))
                        .pt(px(theme::space::SM))
                        .border_t_1()
                        .border_color(theme::border())
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(theme::Text::Small.size())
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::text())
                                        .child(root.tr("贡献者")),
                                )
                                .child(
                                    div()
                                        .text_size(theme::Text::Tiny.size())
                                        .text_color(theme::text_faint())
                                        .child("contrib.rocks"),
                                ),
                        )
                        .child(if let Some(image) = contributors {
                            div()
                                .flex()
                                .w_full()
                                .justify_center()
                                .child(
                                    img(Arc::clone(&image))
                                        .w_full()
                                        .h(px(64.0))
                                        .object_fit(ObjectFit::Contain),
                                )
                                .into_any_element()
                        } else if root.contributors_failed {
                            div()
                                .id("retry-contributors")
                                .h(px(64.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(theme::radius::ROW))
                                .bg(theme::surface_hover())
                                .cursor_pointer()
                                .hover(|style| style.opacity(0.85))
                                .text_size(theme::Text::Tiny.size())
                                .text_color(theme::text_muted())
                                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                    root.ensure_contributors(cx);
                                }))
                                .child(root.tr("加载失败，点击重试"))
                                .into_any_element()
                        } else {
                            div()
                                .h(px(64.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(theme::radius::ROW))
                                .bg(theme::surface_hover())
                                .text_size(theme::Text::Tiny.size())
                                .text_color(theme::text_faint())
                                .child(root.tr("正在加载贡献者…"))
                                .into_any_element()
                        }),
                ),
        )
        .child(settings_card(Some(root.tr("诊断日志"))).child(diagnostic_log_section(root, cx)))
        .into_any_element()
}
