use super::{d2d_color, ArrowEdge};
use windows::{
    Win32::Graphics::Direct2D::Common::D2D1_COLOR_F,
    UI::{
        Color,
        ViewManagement::{UIColorType, UISettings},
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Theme {
    Light,
    Dark,
}

pub(super) fn foreground_for_theme(theme: Theme) -> Color {
    match theme {
        Theme::Light => Color {
            A: 255,
            R: 0,
            G: 0,
            B: 0,
        },
        Theme::Dark => Color {
            A: 255,
            R: 255,
            G: 255,
            B: 255,
        },
    }
}

pub(super) fn theme_from_foreground_rgb(red: u8, green: u8, blue: u8) -> Theme {
    if 5u32 * u32::from(green) + 2u32 * u32::from(red) + u32::from(blue) > 8 * 128 {
        Theme::Dark
    } else {
        Theme::Light
    }
}

pub(super) fn theme_from_ui_settings(
    settings: &UISettings,
) -> windows::core::Result<(Theme, Color)> {
    let foreground = settings.GetColorValue(UIColorType::Foreground)?;
    Ok((
        theme_from_foreground_rgb(foreground.R, foreground.G, foreground.B),
        foreground,
    ))
}

pub(super) fn history_title_color(theme: Theme) -> D2D1_COLOR_F {
    match theme {
        Theme::Light => d2d_color(0, 0, 0, 0.847),
        Theme::Dark => d2d_color(255, 255, 255, 0.847),
    }
}

pub(super) fn popup_base_color(theme: Theme) -> (u8, u8, u8) {
    match theme {
        Theme::Light => (255, 255, 255),
        Theme::Dark => (30, 30, 30),
    }
}

pub(super) fn result_gradient_colors(theme: Theme, edge: ArrowEdge) -> [(u8, u8, u8); 2] {
    let (top, bottom) = match theme {
        Theme::Light => ((244, 247, 249), (188, 205, 216)),
        Theme::Dark => ((0, 0, 0), (55, 66, 73)),
    };
    match edge {
        ArrowEdge::Top => [top, bottom],
        ArrowEdge::Bottom => [bottom, top],
    }
}
