//! The window's normal size after a maximized start. eframe saves the size
//! the window has on exit, so a window closed maximized reopens maximized
//! over a normal size as large as the screen, and leaving maximized then
//! changes nothing but the button. The settings keep the last size and
//! place the window had while not maximized instead, and the first time a
//! window that started maximized leaves it, they are applied.
use super::Workbench;
use eframe::egui;
use serde::{Deserialize, Serialize};

/// Where the window was while neither maximized, minimized nor full screen,
/// in points: its frame's top left and its content's size.
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Debug)]
pub(super) struct NormalWindow {
    position: [f32; 2],
    size: [f32; 2],
}

/// The size used when none was kept, as the window first opens.
const DEFAULT_SIZE: [f32; 2] = [1280., 800.];
/// How many frames a start may take to show the window maximized.
const START_FRAMES: u32 = 60;
/// How many frames a requested size may take to apply, while the window's
/// size is not the normal one to keep.
const SETTLE_FRAMES: u32 = 10;

pub(super) enum WindowState {
    /// Started maximized, not yet shown so.
    Starting { frames: u32 },
    /// Maximized since the start.
    Maximized,
    /// The normal size was just requested.
    Settling { frames: u32 },
    /// The system keeps the normal size from here on; remember it.
    Normal,
}
impl WindowState {
    /// From eframe's saved window state: whether the window closed maximized.
    pub(super) fn new(storage: Option<&dyn eframe::Storage>) -> Self {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Saved {
            maximized: bool,
        }
        let saved: Option<Saved> = storage.and_then(|s| eframe::get_value(s, "window"));
        if saved.is_some_and(|s| s.maximized) {
            Self::Starting { frames: 0 }
        } else {
            Self::Normal
        }
    }
}

impl Workbench {
    /// Follows the window: remembers its normal size, and applies it when a
    /// window that started maximized leaves maximized for the first time.
    pub(super) fn track_window(&mut self, ctx: &egui::Context) {
        if self.smoke.active() {
            return;
        }
        let (maximized, other, inner, outer, monitor) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.maximized,
                v.minimized == Some(true) || v.fullscreen == Some(true),
                v.inner_rect,
                v.outer_rect,
                v.monitor_size,
            )
        });
        let Some(maximized) = maximized else { return };
        self.window = match self.window {
            WindowState::Starting { .. } if maximized => WindowState::Maximized,
            WindowState::Starting { frames } if frames < START_FRAMES => {
                WindowState::Starting { frames: frames + 1 }
            }
            WindowState::Starting { .. } => WindowState::Normal,
            WindowState::Maximized if maximized || other => WindowState::Maximized,
            WindowState::Maximized => {
                let normal = self.settings.normal_window.unwrap_or_else(|| {
                    // Centred on the screen at the size a first start has.
                    let size = egui::Vec2::from(DEFAULT_SIZE);
                    let screen = monitor.unwrap_or(size);
                    let corner = ((screen - size) * 0.5).max(egui::Vec2::ZERO);
                    NormalWindow {
                        position: [corner.x, corner.y],
                        size: DEFAULT_SIZE,
                    }
                });
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(normal.size.into()));
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(normal.position.into()));
                WindowState::Settling { frames: 0 }
            }
            WindowState::Settling { frames } if frames < SETTLE_FRAMES => {
                WindowState::Settling { frames: frames + 1 }
            }
            WindowState::Settling { .. } | WindowState::Normal => WindowState::Normal,
        };
        if matches!(self.window, WindowState::Normal)
            && !maximized
            && !other
            && let (Some(inner), Some(outer)) = (inner, outer)
        {
            self.settings.normal_window = Some(NormalWindow {
                position: [outer.min.x, outer.min.y],
                size: [inner.width(), inner.height()],
            });
        }
    }
}
