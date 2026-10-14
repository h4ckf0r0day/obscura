//! SVG geometry in user units, independent of rasterization and element transforms.
use kurbo::{Arc, CubicBez, Ellipse, Line, ParamCurveArclen, Point, QuadBez, Shape, SvgArc};
use obscura_dom::{DomTree, NodeId};
use svgtypes::{Length, LengthUnit, PathParser, PathSegment, PointsParser, ViewBox};

const ACCURACY: f64 = 0.001;

// Presentation attributes and inline geometry declarations are read on every
// call so attribute/style changes cannot leave a stale geometry result.
fn property(dom: &DomTree, id: NodeId, name: &str) -> Option<String> {
    dom.with_node(id, |node| {
        let inline = node.get_attribute("style").and_then(|style| {
            style
                .split(';')
                .filter_map(|declaration| {
                    let (key, value) = declaration.split_once(':')?;
                    (key.trim().eq_ignore_ascii_case(name)).then(|| {
                        value
                            .trim()
                            .strip_suffix("!important")
                            .unwrap_or(value.trim())
                            .trim()
                            .to_owned()
                    })
                })
                .last()
        });
        inline.or_else(|| node.get_attribute(name).map(str::to_owned))
    })
    .flatten()
}

fn resolve(value: &str, basis: f64, font_size: f64) -> Option<f64> {
    let length: Length = value.parse().ok()?;
    let factor = match length.unit {
        LengthUnit::None | LengthUnit::Px => 1.0,
        LengthUnit::Percent => basis / 100.0,
        LengthUnit::In => 96.0,
        LengthUnit::Cm => 96.0 / 2.54,
        LengthUnit::Mm => 96.0 / 25.4,
        LengthUnit::Pt => 96.0 / 72.0,
        LengthUnit::Pc => 16.0,
        LengthUnit::Em => font_size,
        LengthUnit::Ex => font_size / 2.0,
    };
    let value = length.number * factor;
    value.is_finite().then_some(value)
}

fn context(dom: &DomTree, id: NodeId) -> (f64, f64, f64) {
    let mut ancestors = Vec::new();
    let mut next = Some(id);
    while let Some(id) = next {
        ancestors.push(id);
        next = dom.with_node(id, |node| node.parent).flatten();
    }
    let mut font = 16.0;
    let (mut width, mut height) = (300.0, 150.0);
    for id in ancestors.into_iter().rev() {
        if let Some(value) = property(dom, id, "font-size") {
            font = resolve(&value, font, font)
                .filter(|size| *size > 0.0)
                .unwrap_or(font);
        }
        let svg = dom
            .with_node(id, |node| {
                node.as_element().is_some_and(|name| {
                    name.ns.as_ref() == "http://www.w3.org/2000/svg" && name.local.as_ref() == "svg"
                })
            })
            .unwrap_or(false);
        if svg {
            let w = property(dom, id, "width").and_then(|v| resolve(&v, width, font));
            let h = property(dom, id, "height").and_then(|v| resolve(&v, height, font));
            width = w.unwrap_or(width);
            height = h.unwrap_or(height);
            if let Some(view_box) =
                property(dom, id, "viewBox").and_then(|v| v.parse::<ViewBox>().ok())
            {
                width = view_box.w;
                height = view_box.h;
            }
        }
    }
    (width, height, font)
}

fn path_length(data: &str) -> f64 {
    let mut current = Point::ZERO;
    let mut start = Point::ZERO;
    let mut cubic_control = None;
    let mut quadratic_control = None;
    let mut length = 0.0;
    for segment in PathParser::from(data) {
        // SVG renders the valid prefix when path data has a syntax error.
        let Ok(segment) = segment else { break };
        let point = |abs, x, y| {
            if abs {
                Point::new(x, y)
            } else {
                Point::new(current.x + x, current.y + y)
            }
        };
        let mut next_cubic = None;
        let mut next_quadratic = None;
        match segment {
            PathSegment::MoveTo { abs, x, y } => {
                current = point(abs, x, y);
                start = current;
            }
            PathSegment::LineTo { abs, x, y } => {
                let end = point(abs, x, y);
                length += Line::new(current, end).arclen(ACCURACY);
                current = end;
            }
            PathSegment::HorizontalLineTo { abs, x } => {
                let end = Point::new(if abs { x } else { current.x + x }, current.y);
                length += Line::new(current, end).arclen(ACCURACY);
                current = end;
            }
            PathSegment::VerticalLineTo { abs, y } => {
                let end = Point::new(current.x, if abs { y } else { current.y + y });
                length += Line::new(current, end).arclen(ACCURACY);
                current = end;
            }
            PathSegment::CurveTo {
                abs,
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let c1 = point(abs, x1, y1);
                let c2 = point(abs, x2, y2);
                let end = point(abs, x, y);
                length += CubicBez::new(current, c1, c2, end).arclen(ACCURACY);
                next_cubic = Some(c2);
                current = end;
            }
            PathSegment::SmoothCurveTo { abs, x2, y2, x, y } => {
                let c1 = cubic_control
                    .map(|control| current + (current - control))
                    .unwrap_or(current);
                let c2 = point(abs, x2, y2);
                let end = point(abs, x, y);
                length += CubicBez::new(current, c1, c2, end).arclen(ACCURACY);
                next_cubic = Some(c2);
                current = end;
            }
            PathSegment::Quadratic { abs, x1, y1, x, y } => {
                let control = point(abs, x1, y1);
                let end = point(abs, x, y);
                length += QuadBez::new(current, control, end).arclen(ACCURACY);
                next_quadratic = Some(control);
                current = end;
            }
            PathSegment::SmoothQuadratic { abs, x, y } => {
                let control = quadratic_control
                    .map(|control| current + (current - control))
                    .unwrap_or(current);
                let end = point(abs, x, y);
                length += QuadBez::new(current, control, end).arclen(ACCURACY);
                next_quadratic = Some(control);
                current = end;
            }
            PathSegment::EllipticalArc {
                abs,
                rx,
                ry,
                x_axis_rotation,
                large_arc,
                sweep,
                x,
                y,
            } => {
                let end = point(abs, x, y);
                if let Some(arc) = Arc::from_svg_arc(&SvgArc {
                    from: current,
                    to: end,
                    radii: (rx, ry).into(),
                    x_rotation: x_axis_rotation.to_radians(),
                    large_arc,
                    sweep,
                }) {
                    arc.to_cubic_beziers(ACCURACY * 0.1, |c1, c2, end| {
                        length += CubicBez::new(current, c1, c2, end).arclen(ACCURACY);
                        current = end;
                    });
                } else {
                    length += Line::new(current, end).arclen(ACCURACY);
                }
                current = end;
            }
            PathSegment::ClosePath { .. } => {
                length += Line::new(current, start).arclen(ACCURACY);
                current = start;
            }
        }
        cubic_control = next_cubic;
        quadratic_control = next_quadratic;
    }
    length
}

pub(crate) fn total_length(dom: &DomTree, id: NodeId) -> Option<f64> {
    let (tag, connected) = dom
        .with_node(id, |node| {
            node.as_element()
                .filter(|name| name.ns.as_ref() == "http://www.w3.org/2000/svg")
                .map(|name| (name.local.to_string(), node.connected))
        })
        .flatten()?;
    if !connected && tag != "path" {
        return None;
    }
    let length = if tag == "path" {
        let data = property(dom, id, "d").unwrap_or_default();
        let data = data.trim();
        let data = data
            .strip_prefix("path(")
            .and_then(|v| v.strip_suffix(')'))
            .map(|v| v.trim().trim_matches(['\'', '"']))
            .unwrap_or(data);
        path_length(data)
    } else {
        let (width, height, font) = context(dom, id);
        let value = |name, basis| {
            property(dom, id, name)
                .and_then(|v| resolve(&v, basis, font))
                .unwrap_or(0.0)
        };
        match tag.as_str() {
            "line" => (value("x2", width) - value("x1", width))
                .hypot(value("y2", height) - value("y1", height)),
            "circle" => {
                std::f64::consts::TAU * value("r", width.hypot(height) / 2.0_f64.sqrt()).max(0.0)
            }
            "ellipse" => Ellipse::new(
                (0.0, 0.0),
                (value("rx", width).max(0.0), value("ry", height).max(0.0)),
                0.0,
            )
            .perimeter(ACCURACY),
            "rect" => {
                let w = value("width", width).max(0.0);
                let h = value("height", height).max(0.0);
                let rx = property(dom, id, "rx").and_then(|v| resolve(&v, width, font));
                let ry = property(dom, id, "ry").and_then(|v| resolve(&v, height, font));
                let rx = rx.or(ry).unwrap_or(0.0).max(0.0).min(w / 2.0);
                let ry = ry
                    .or_else(|| property(dom, id, "rx").and_then(|v| resolve(&v, width, font)))
                    .unwrap_or(0.0)
                    .max(0.0)
                    .min(h / 2.0);
                if rx == 0.0 || ry == 0.0 {
                    2.0 * (w + h)
                } else {
                    2.0 * (w + h) - 4.0 * (rx + ry)
                        + Ellipse::new((0.0, 0.0), (rx, ry), 0.0).perimeter(ACCURACY)
                }
            }
            "polyline" | "polygon" => {
                let data = property(dom, id, "points").unwrap_or_default();
                let mut points = PointsParser::from(data.as_str());
                let Some(first) = points.next() else {
                    return Some(0.0);
                };
                let mut previous = first;
                let mut length = 0.0;
                for point in points {
                    length += (point.0 - previous.0).hypot(point.1 - previous.1);
                    previous = point;
                }
                if tag == "polygon" {
                    length += (previous.0 - first.0).hypot(previous.1 - first.1);
                }
                length
            }
            _ => return None,
        }
    };
    let length = length as f32;
    length.is_finite().then_some(f64::from(length))
}
