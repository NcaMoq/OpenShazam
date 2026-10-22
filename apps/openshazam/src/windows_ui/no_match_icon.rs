#![deny(unsafe_op_in_unsafe_fn)]

use super::D2DCompositionGeometrySource;
use windows::{
    core::Interface,
    Graphics::IGeometrySource2D,
    Win32::Graphics::Direct2D::{
        Common::{D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED},
        ID2D1Factory, ID2D1Geometry,
    },
    UI::Composition::{CompositionPath, CompositionPathGeometry, Compositor},
};
use windows_numerics::Vector2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum NoMatchIconCommand {
    Move((f64, f64)),
    Line((f64, f64)),
    Cubic((f64, f64), (f64, f64), (f64, f64)),
    Close,
}
// UI-168: ARM64 and x86_64 constants agree for all 34 path commands. Keep
// duplicate line segments and out-of-range control points as recorded.
pub(super) const NO_MATCH_ICON_PATH: [NoMatchIconCommand; 34] = [
    NoMatchIconCommand::Move((0.499499989, 0.804914556)),
    NoMatchIconCommand::Cubic(
        (0.657000017, 0.804914556),
        (0.784749985, 0.848632486),
        (0.784749985, 0.902521378),
    ),
    NoMatchIconCommand::Cubic(
        (0.784749985, 0.956367493),
        (0.657000017, 1.000085488),
        (0.499499989, 1.000085488),
    ),
    NoMatchIconCommand::Cubic(
        (0.341875005, 1.000085488),
        (0.214000010, 0.956367493),
        (0.214000010, 0.902521378),
    ),
    NoMatchIconCommand::Cubic(
        (0.214000010, 0.848632486),
        (0.341875005, 0.804914556),
        (0.499499989, 0.804914556),
    ),
    NoMatchIconCommand::Close,
    NoMatchIconCommand::Move((0.686124992, 0.624957288)),
    NoMatchIconCommand::Line((0.686124992, 0.625042712)),
    NoMatchIconCommand::Line((0.686124992, 0.625085488)),
    NoMatchIconCommand::Line((0.686124992, 0.625085488)),
    NoMatchIconCommand::Line((0.686000013, 0.625170911)),
    NoMatchIconCommand::Line((0.686000013, 0.625085488)),
    NoMatchIconCommand::Line((0.686000013, 0.625170911)),
    NoMatchIconCommand::Line((0.686000013, 0.625213688)),
    NoMatchIconCommand::Line((0.685874987, 0.625299177)),
    NoMatchIconCommand::Line((0.685874987, 0.625384600)),
    NoMatchIconCommand::Line((0.685750008, 0.625427377)),
    NoMatchIconCommand::Line((0.684375000, 0.627307664)),
    NoMatchIconCommand::Cubic(
        (0.676875019, 0.636923048),
        (0.666625023, 0.645897433),
        (0.651000023, 0.653931642),
    ),
    NoMatchIconCommand::Line((0.647249985, 0.655726506)),
    NoMatchIconCommand::Cubic(
        (0.585750008, 0.685256436),
        (0.473124981, 0.691794893),
        (0.392374992, 0.670341883),
    ),
    NoMatchIconCommand::Cubic(
        (0.317499995, 0.650341849),
        (0.301749992, 0.611923087),
        (0.278125000, 0.580128205),
    ),
    NoMatchIconCommand::Cubic(
        (0.225500011, 0.518247849),
        (0.178999996, 0.455170949),
        (0.132000005, 0.392606849),
    ),
    NoMatchIconCommand::Cubic(
        (0.105999994, 0.361495711),
        (0.082249999, 0.329957261),
        (0.058499998, 0.298589755),
    ),
    NoMatchIconCommand::Cubic(
        (0.027750000, 0.261025649),
        (-0.005125000, 0.223290598),
        (0.000625000, 0.183247868),
    ),
    NoMatchIconCommand::Cubic(
        (0.011125000, 0.111623935),
        (0.125999999, 0.046581195),
        (0.293875003, 0.016965812),
    ),
    NoMatchIconCommand::Cubic(
        (0.464624977, -0.013076923),
        (0.669999981, -0.002606838),
        (0.814999962, 0.043504275),
    ),
    NoMatchIconCommand::Cubic(
        (0.968750000, 0.092350430),
        (1.034624958, 0.174017099),
        (0.979749966, 0.248376064),
    ),
    NoMatchIconCommand::Cubic(
        (0.932999992, 0.311666668),
        (0.883874989, 0.375042744),
        (0.831999969, 0.437692300),
    ),
    NoMatchIconCommand::Cubic(
        (0.784250021, 0.500128200),
        (0.738875008, 0.563247876),
        (0.686124992, 0.625085488),
    ),
    NoMatchIconCommand::Line((0.686124992, 0.625042712)),
    NoMatchIconCommand::Line((0.686124992, 0.624957288)),
    NoMatchIconCommand::Line((0.686124992, 0.624957288)),
    NoMatchIconCommand::Close,
];

const NO_MATCH_ICON_BASE_WIDTH: f32 = 40.0;
const NO_MATCH_ICON_BASE_HEIGHT: f32 = 117.0;
const NO_MATCH_ICON_SCALE: f32 = 0.8;
pub(super) fn no_match_icon_rect(button_size: f32) -> [f32; 4] {
    let width = NO_MATCH_ICON_BASE_WIDTH * NO_MATCH_ICON_SCALE;
    let height = NO_MATCH_ICON_BASE_HEIGHT * NO_MATCH_ICON_SCALE;
    let left = (button_size - width) * 0.5;
    let top = (button_size - height) * 0.5;
    [left, top, left + width, top + height]
}

fn no_match_icon_point(
    point: (f64, f64),
    width: f32,
    height: f32,
    origin_x: f32,
    origin_y: f32,
) -> Vector2 {
    Vector2 {
        X: (f64::from(origin_x) + point.0 * f64::from(width)) as f32,
        Y: (f64::from(origin_y) + point.1 * f64::from(height)) as f32,
    }
}

/// Build the Composition path from the native no-match icon's vector commands.
///
/// # Safety
/// `compositor` and `d2d_factory` must be live COM objects usable on the
/// current apartment thread, and `icon_rect` must have finite, positive
/// dimensions. Direct2D geometry sinks are FFI interfaces and require this
/// thread and input contract for the duration of the calls.
pub(super) unsafe fn make_no_match_icon_geometry(
    compositor: &Compositor,
    d2d_factory: &ID2D1Factory,
    icon_rect: [f32; 4],
) -> windows::core::Result<CompositionPathGeometry> {
    let origin_x = icon_rect[0];
    let origin_y = icon_rect[1];
    let width = icon_rect[2] - icon_rect[0];
    let height = icon_rect[3] - icon_rect[1];
    // SAFETY: The caller guarantees live Direct2D/Composition COM interfaces
    // on their owning apartment thread and finite, positive icon bounds. These
    // COM calls are synchronous and all geometry sinks stay alive until closed.
    unsafe {
        let source = d2d_factory.CreatePathGeometry()?;
        let sink = source.Open()?;
        for command in NO_MATCH_ICON_PATH {
            match command {
                NoMatchIconCommand::Move(point) => {
                    sink.BeginFigure(
                        no_match_icon_point(point, width, height, origin_x, origin_y),
                        D2D1_FIGURE_BEGIN_FILLED,
                    );
                }
                NoMatchIconCommand::Line(point) => {
                    sink.AddLine(no_match_icon_point(
                        point, width, height, origin_x, origin_y,
                    ));
                }
                NoMatchIconCommand::Cubic(cp1, cp2, end) => {
                    sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                        point1: no_match_icon_point(cp1, width, height, origin_x, origin_y),
                        point2: no_match_icon_point(cp2, width, height, origin_x, origin_y),
                        point3: no_match_icon_point(end, width, height, origin_x, origin_y),
                    });
                }
                NoMatchIconCommand::Close => sink.EndFigure(D2D1_FIGURE_END_CLOSED),
            }
        }
        sink.Close()?;

        // UI-168 finalIconPathFromIconPath:inRect: rotates around the path's own
        // bounds center, then subtracts the supplied rect origin. Apply those
        // operations to the full source coordinates before creating the cached
        // Composition geometry.
        let bounds = source.GetBounds(None)?;
        let center_x = (bounds.left + bounds.right) * 0.5;
        let center_y = (bounds.top + bounds.bottom) * 0.5;
        let geometry = d2d_factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        for command in NO_MATCH_ICON_PATH {
            let transform = |point: (f64, f64)| {
                let point = (
                    f64::from(origin_x) + point.0 * f64::from(width),
                    f64::from(origin_y) + point.1 * f64::from(height),
                );
                Vector2 {
                    X: (2.0 * f64::from(center_x) - point.0 - f64::from(origin_x)) as f32,
                    Y: (2.0 * f64::from(center_y) - point.1 - f64::from(origin_y)) as f32,
                }
            };
            match command {
                NoMatchIconCommand::Move(point) => {
                    sink.BeginFigure(transform(point), D2D1_FIGURE_BEGIN_FILLED);
                }
                NoMatchIconCommand::Line(point) => sink.AddLine(transform(point)),
                NoMatchIconCommand::Cubic(cp1, cp2, end) => {
                    sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                        point1: transform(cp1),
                        point2: transform(cp2),
                        point3: transform(end),
                    });
                }
                NoMatchIconCommand::Close => sink.EndFigure(D2D1_FIGURE_END_CLOSED),
            }
        }
        sink.Close()?;
        let geometry: ID2D1Geometry = geometry.cast()?;
        let source: IGeometrySource2D = D2DCompositionGeometrySource::new(geometry).into();
        compositor.CreatePathGeometryWithPath(&CompositionPath::Create(&source)?)
    }
}
