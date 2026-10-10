//! Alignment of tracks and final positioning of items
use super::types::GridTrack;
use crate::compute::common::alignment::{
    apply_alignment_fallback, compute_alignment_offset, resolve_self_alignment_safety,
};
use crate::geometry::{InBothAbsAxis, Line, Point, Rect, Size};
use crate::style::{
    AlignContent, AlignItems, AlignItemsKeyword, AlignSelf, AvailableSpace, CoreStyle, GridItemStyle, Overflow,
    Position,
};
use crate::tree::{Layout, LayoutPartialTreeExt, NodeId, SizingMode};
use crate::util::sys::{f32_max, f32_min};
use crate::util::{MaybeMath, MaybeResolve, ResolveOrZero};

#[cfg(feature = "content_size")]
use crate::compute::common::content_size::compute_content_size_contribution;
use crate::{BoxSizing, Direction, LayoutGridContainer};

/// Align the grid tracks within the grid according to the align-content (rows) or
/// justify-content (columns) property. This only does anything if the size of the
/// grid is not equal to the size of the grid container in the axis being aligned.
pub(super) fn align_tracks(
    grid_container_content_box_size: f32,
    padding: Line<f32>,
    border: Line<f32>,
    tracks: &mut [GridTrack],
    track_alignment_style: AlignContent,
    axis_is_reversed: bool,
) {
    let used_size: f32 = tracks.iter().map(|track| track.base_size).sum();
    let free_space = grid_container_content_box_size - used_size;
    let origin = padding.start + border.start;

    // Count the number of non-collapsed tracks (not counting gutters)
    let num_tracks = tracks.iter().skip(1).step_by(2).filter(|track| !track.is_collapsed).count();

    // Grid layout treats gaps as full tracks rather than applying them at alignment so we
    // simply pass zero here. Grid layout is never reversed.
    let gap = 0.0;
    let layout_is_reversed = false;
    let track_alignment = apply_alignment_fallback(free_space, num_tracks, track_alignment_style);
    let track_alignment = if axis_is_reversed { track_alignment.reversed() } else { track_alignment };

    // Compute offsets
    let mut total_offset = origin;
    let mut seen_non_collapsed_track = false;
    tracks.iter_mut().enumerate().for_each(|(i, track)| {
        // Odd tracks are gutters (but slices are zero-indexed, so odd tracks have even indices)
        let is_gutter = i % 2 == 0;
        let is_non_collapsed_track = !is_gutter && !track.is_collapsed;

        // Alignment offsets should be applied only to non-collapsed tracks.
        let is_first = is_non_collapsed_track && !seen_non_collapsed_track;

        let offset = if is_non_collapsed_track {
            compute_alignment_offset(free_space, num_tracks, gap, track_alignment, layout_is_reversed, is_first)
        } else {
            0.0
        };

        track.offset = total_offset + offset;
        total_offset = total_offset + offset + track.base_size;
        if is_non_collapsed_track {
            seen_non_collapsed_track = true;
        }
    });
}

/// Align and size a grid item into it's final position
pub(super) fn align_and_position_item(
    tree: &mut impl LayoutGridContainer,
    node: NodeId,
    order: u32,
    grid_area: Rect<f32>,
    container_alignment_styles: InBothAbsAxis<Option<AlignItems>>,
    baseline_shim: f32,
    direction: Direction,
) -> (Size<f32>, f32, f32) {
    let grid_area_size = Size { width: grid_area.right - grid_area.left, height: grid_area.bottom - grid_area.top };

    let style = tree.get_grid_child_style(node);

    let overflow = style.overflow();
    let scrollbar_width = style.scrollbar_width();
    let aspect_ratio = style.aspect_ratio();
    let justify_self = style.justify_self();
    let align_self = style.align_self();

    let position = style.position();
    let inset_horizontal = style
        .inset()
        .horizontal_components()
        .map(|size| size.resolve_to_option(grid_area_size.width, |val, basis| tree.calc(val, basis)));
    let inset_vertical = style
        .inset()
        .vertical_components()
        .map(|size| size.resolve_to_option(grid_area_size.height, |val, basis| tree.calc(val, basis)));
    let padding =
        style.padding().map(|p| p.resolve_or_zero(Some(grid_area_size.width), |val, basis| tree.calc(val, basis)));
    let border =
        style.border().map(|p| p.resolve_or_zero(Some(grid_area_size.width), |val, basis| tree.calc(val, basis)));
    let padding_border_size = (padding + border).sum_axes();

    let box_sizing_adjustment =
        if style.box_sizing() == BoxSizing::ContentBox { padding_border_size } else { Size::ZERO };
    let aspect_ratio_adjustment =
        if style.aspect_ratio_uses_content_box() { padding_border_size } else { box_sizing_adjustment };

    let inherent_size = style
        .size()
        .maybe_resolve(grid_area_size, |val, basis| tree.calc(val, basis))
        .maybe_add(box_sizing_adjustment);
    let min_size = style
        .min_size()
        .maybe_resolve(grid_area_size, |val, basis| tree.calc(val, basis))
        .maybe_add(box_sizing_adjustment)
        .or(padding_border_size.map(Some))
        .maybe_max(padding_border_size)
        .maybe_apply_aspect_ratio(aspect_ratio);
    let max_size = style
        .max_size()
        .maybe_resolve(grid_area_size, |val, basis| tree.calc(val, basis))
        .maybe_apply_aspect_ratio(aspect_ratio)
        .maybe_add(box_sizing_adjustment);

    // Preserve `normal` provenance until both axes and the preferred aspect
    // ratio are known. Explicit stretch in one axis can make the other normal
    // axis ratio-derived, while two normal axes prefer inline-axis stretch.
    let alignment_styles = super::resolve_item_alignment(
        justify_self.or(container_alignment_styles.horizontal).unwrap_or(AlignSelf::NORMAL),
        align_self.or(container_alignment_styles.vertical).unwrap_or(AlignSelf::NORMAL),
        style.is_compressible_replaced(),
        aspect_ratio.is_some(),
    );

    // Note: This is not a bug. It is part of the CSS spec that both horizontal and vertical margins
    // resolve against the WIDTH of the grid area.
    let margin =
        style.margin().map(|margin| margin.resolve_to_option(grid_area_size.width, |val, basis| tree.calc(val, basis)));

    let grid_area_minus_item_margins_size = Size {
        width: grid_area_size.width.maybe_sub(margin.left).maybe_sub(margin.right),
        height: grid_area_size.height.maybe_sub(margin.top).maybe_sub(margin.bottom) - baseline_shim,
    };

    // If node is absolutely positioned and width is not set explicitly, then deduce it
    // from left, right and container_content_box if both are set.
    let width = inherent_size.width.or_else(|| {
        // Apply width derived from both the left and right properties of an absolutely
        // positioned element being set
        if position == Position::Absolute {
            if let (Some(left), Some(right)) = (inset_horizontal.start, inset_horizontal.end) {
                return Some(f32_max(grid_area_minus_item_margins_size.width - left - right, 0.0));
            }
        }

        // Apply width based on stretch alignment if:
        //  - Alignment style is "stretch"
        //  - The node is not absolutely positioned
        //  - The node does not have auto margins in this axis.
        if margin.left.is_some()
            && margin.right.is_some()
            && alignment_styles.horizontal == AlignSelf::STRETCH
            && position != Position::Absolute
        {
            return Some(grid_area_minus_item_margins_size.width);
        }

        None
    });

    let height = inherent_size.height.or_else(|| {
        if position == Position::Absolute {
            if let (Some(top), Some(bottom)) = (inset_vertical.start, inset_vertical.end) {
                return Some(f32_max(grid_area_minus_item_margins_size.height - top - bottom, 0.0));
            }
        }

        // Apply height based on stretch alignment if:
        //  - Alignment style is "stretch"
        //  - The node is not absolutely positioned
        //  - The node does not have auto margins in this axis.
        if margin.top.is_some()
            && margin.bottom.is_some()
            && alignment_styles.vertical == AlignSelf::STRETCH
            && position != Position::Absolute
        {
            return Some(grid_area_minus_item_margins_size.height);
        }

        None
    });
    // Stretch is resolved before preferred-ratio transfer. If both axes are
    // stretched (or otherwise definite), the ratio does not overwrite either
    // one. If only one axis is definite, it supplies the other through the
    // ratio. This matches replaced grid sizing in Blink and Gecko.
    let Size { width, height } =
        super::apply_preferred_aspect_ratio(Size { width, height }, aspect_ratio, aspect_ratio_adjustment);

    // Clamp size by min and max width/height
    let Size { width, height } = Size { width, height }.maybe_clamp(min_size, max_size);

    // Auto margins disable self-alignment in their axis. In particular, an
    // auto inline margin disables the default `stretch`, so an auto-width
    // in-flow item must use fit-content sizing even though its resolved
    // justify-self value is still `stretch`.
    //
    // At this point a genuinely stretched (or otherwise definite) width is
    // `Some`, while an auto width whose effective alignment is non-stretch is
    // `None`. Absolutely-positioned items use their separate shrink-to-fit
    // path below.
    let uses_inline_fit_content = position != Position::Absolute && width.is_none();

    // Layout node
    drop(style);

    let size = if position == Position::Absolute && (width.is_none() || height.is_none()) {
        tree.measure_child_size_both(
            node,
            Size { width, height },
            grid_area_size.map(Option::Some),
            grid_area_minus_item_margins_size.map(AvailableSpace::Definite),
            SizingMode::InherentSize,
            Line::FALSE,
        )
        .map(Some)
    } else {
        Size { width, height }
    };

    let mut layout_output = tree.perform_child_layout(
        node,
        size,
        grid_area_size.map(Option::Some),
        grid_area_minus_item_margins_size.map(AvailableSpace::Definite),
        SizingMode::InherentSize,
        Line::FALSE,
    );

    // Resolve final size
    let mut resolved_size = size.unwrap_or(layout_output.size).maybe_clamp(min_size, max_size);

    // An auto-sized grid item that is not effectively stretched uses
    // fit-content sizing in the inline axis:
    //
    //   max(min-content, min(available, max-content))
    //
    // The inherent-size layout above obtains the max-content size. Only pay
    // for a min-content measurement when the available width would actually
    // clamp it. This preserves intrinsic overflow for unbreakable content,
    // while breakable content is laid out again at the available width.
    if uses_inline_fit_content && resolved_size.width > grid_area_minus_item_margins_size.width {
        let min_content_width = tree.measure_child_size(
            node,
            Size { width: None, height: size.height },
            grid_area_size.map(Option::Some),
            Size {
                width: AvailableSpace::MinContent,
                height: AvailableSpace::Definite(grid_area_minus_item_margins_size.height),
            },
            SizingMode::InherentSize,
            crate::AbsoluteAxis::Horizontal,
            Line::FALSE,
        );
        let fit_content_width =
            f32_max(min_content_width, f32_min(grid_area_minus_item_margins_size.width, resolved_size.width))
                .maybe_clamp(min_size.width, max_size.width);
        let constrained_size = Size { width: Some(fit_content_width), height: size.height };
        layout_output = tree.perform_child_layout(
            node,
            constrained_size,
            grid_area_size.map(Option::Some),
            grid_area_minus_item_margins_size.map(AvailableSpace::Definite),
            SizingMode::InherentSize,
            Line::FALSE,
        );
        resolved_size = constrained_size.unwrap_or(layout_output.size).maybe_clamp(min_size, max_size);
    }

    let Size { width, height } = resolved_size;

    let (x, x_margin) = align_item_within_area(
        Line { start: grid_area.left, end: grid_area.right },
        alignment_styles.horizontal,
        width,
        position,
        inset_horizontal,
        margin.horizontal_components(),
        0.0,
        direction,
    );
    let (y, y_margin) = align_item_within_area(
        Line { start: grid_area.top, end: grid_area.bottom },
        alignment_styles.vertical,
        height,
        position,
        inset_vertical,
        margin.vertical_components(),
        baseline_shim,
        Direction::Ltr,
    );

    let scrollbar_size = Size {
        width: if overflow.y == Overflow::Scroll { scrollbar_width } else { 0.0 },
        height: if overflow.x == Overflow::Scroll { scrollbar_width } else { 0.0 },
    };

    let resolved_margin = Rect { left: x_margin.start, right: x_margin.end, top: y_margin.start, bottom: y_margin.end };

    tree.set_unrounded_layout(
        node,
        &Layout {
            order,
            location: Point { x, y },
            size: Size { width, height },
            #[cfg(feature = "content_size")]
            content_size: layout_output.content_size,
            scrollbar_size,
            padding,
            border,
            margin: resolved_margin,
        },
    );

    #[cfg(feature = "content_size")]
    let contribution = compute_content_size_contribution(
        Point { x: x - grid_area.left, y: y - grid_area.top },
        Size { width, height },
        layout_output.content_size,
        overflow,
    );
    #[cfg(not(feature = "content_size"))]
    let contribution = Size::ZERO;

    (contribution, y, height)
}

/// Align and size a grid item along a single axis
#[allow(clippy::too_many_arguments)]
pub(super) fn align_item_within_area(
    grid_area: Line<f32>,
    alignment_style: AlignSelf,
    resolved_size: f32,
    position: Position,
    inset: Line<Option<f32>>,
    margin: Line<Option<f32>>,
    baseline_shim: f32,
    direction: Direction,
) -> (f32, Line<f32>) {
    // Calculate grid area dimension in the axis
    let non_auto_margin = Line { start: margin.start.unwrap_or(0.0) + baseline_shim, end: margin.end.unwrap_or(0.0) };
    let grid_area_size = f32_max(grid_area.end - grid_area.start, 0.0);
    let free_space = f32_max(grid_area_size - resolved_size - non_auto_margin.sum(), 0.0);

    // Expand auto margins to fill available space
    let auto_margin_count = margin.start.is_none() as u8 + margin.end.is_none() as u8;
    let auto_margin_size = if auto_margin_count > 0 { free_space / auto_margin_count as f32 } else { 0.0 };
    let resolved_margin = Line {
        start: margin.start.unwrap_or(auto_margin_size) + baseline_shim,
        end: margin.end.unwrap_or(auto_margin_size),
    };

    let overflows = resolved_size + non_auto_margin.sum() > grid_area_size;
    // In-flow auto margins take precedence over self-alignment and are always
    // safe. When the item overflows there is no positive free space for the
    // auto margins to absorb, so safe alignment falls back to the logical
    // start edge rather than honoring an authored unsafe center/end alignment.
    // Absolutely positioned items use their authored self-alignment here;
    // their auto margins are resolved by the abs-pos constraint equation.
    let alignment_keyword = if position != Position::Absolute
        && auto_margin_count > 0
        && overflows
    {
        AlignItemsKeyword::Start
    } else {
        resolve_self_alignment_safety(alignment_style, overflows)
    };

    // Compute offset in the axis
    let alignment_based_offset = match alignment_keyword {
        // TODO: Add support for baseline alignment. For now we treat it as "start".
        AlignItemsKeyword::Normal
        | AlignItemsKeyword::Start
        | AlignItemsKeyword::FlexStart
        | AlignItemsKeyword::Baseline
        | AlignItemsKeyword::Stretch => {
            if direction.is_rtl() {
                grid_area_size - resolved_size - resolved_margin.end
            } else {
                resolved_margin.start
            }
        }
        AlignItemsKeyword::End | AlignItemsKeyword::FlexEnd => {
            if direction.is_rtl() {
                resolved_margin.start
            } else {
                grid_area_size - resolved_size - resolved_margin.end
            }
        }
        AlignItemsKeyword::Center => {
            (grid_area_size - resolved_size + resolved_margin.start - resolved_margin.end) / 2.0
        }
    };

    let offset_within_area = if position == Position::Absolute {
        match (inset.start, inset.end) {
            (Some(start), Some(end)) => {
                if direction.is_rtl() {
                    grid_area_size - end - resolved_size - non_auto_margin.end
                } else {
                    start + non_auto_margin.start
                }
            }
            (Some(start), None) => start + non_auto_margin.start,
            (None, Some(end)) => grid_area_size - end - resolved_size - non_auto_margin.end,
            (None, None) => alignment_based_offset,
        }
    } else {
        alignment_based_offset
    };

    let mut start = grid_area.start + offset_within_area;
    if position == Position::Relative {
        let relative_inset = if direction.is_rtl() {
            inset.end.map(|pos| -pos).or(inset.start)
        } else {
            inset.start.or(inset.end.map(|pos| -pos))
        };
        start += relative_inset.unwrap_or(0.0);
    }

    (start, resolved_margin)
}

#[cfg(test)]
mod tests {
    use super::{f32_max, f32_min};
    use crate::prelude::*;
    use crate::Point;

    #[derive(Clone, Copy)]
    struct IntrinsicInlineSize {
        min: f32,
        max: f32,
    }

    fn measure_intrinsic_child(
        known_dimensions: Size<Option<f32>>,
        available_space: Size<AvailableSpace>,
        _node: NodeId,
        context: Option<&mut IntrinsicInlineSize>,
        _style: &Style,
    ) -> Size<f32> {
        let intrinsic = context.copied().unwrap_or(IntrinsicInlineSize { min: 0.0, max: 0.0 });
        let width = known_dimensions.width.unwrap_or_else(|| match available_space.width {
            AvailableSpace::MinContent => intrinsic.min,
            AvailableSpace::MaxContent => intrinsic.max,
            AvailableSpace::Definite(available) => f32_max(intrinsic.min, f32_min(available, intrinsic.max)),
        });
        Size { width, height: known_dimensions.height.unwrap_or(10.0) }
    }

    #[derive(Clone, Copy)]
    struct ReplacedIntrinsicSize {
        width: f32,
        height: f32,
    }

    fn measure_replaced_child(
        known_dimensions: Size<Option<f32>>,
        _available_space: Size<AvailableSpace>,
        _node: NodeId,
        context: Option<&mut ReplacedIntrinsicSize>,
        _style: &Style,
    ) -> Size<f32> {
        let intrinsic = context.copied().unwrap_or(ReplacedIntrinsicSize { width: 0.0, height: 0.0 });
        let ratio = intrinsic.width / intrinsic.height;
        match known_dimensions {
            Size { width: Some(width), height: Some(height) } => Size { width, height },
            Size { width: Some(width), height: None } => Size { width, height: width / ratio },
            Size { width: None, height: Some(height) } => Size { width: height * ratio, height },
            Size { width: None, height: None } => Size { width: intrinsic.width, height: intrinsic.height },
        }
    }

    fn layout_replaced_grid_item(mut item_style: Style, mut root_style: Style) -> (Point<f32>, Size<f32>) {
        let mut tree = TaffyTree::new();
        tree.disable_rounding();
        item_style.aspect_ratio = Some(2.0);
        let item = tree
            .new_leaf_with_context(item_style, ReplacedIntrinsicSize { width: 100.0, height: 50.0 })
            .unwrap();
        root_style.display = Display::Grid;
        root_style.size = Size { width: length(300.0), height: length(200.0) };
        root_style.grid_template_columns = vec![length(300.0)];
        root_style.grid_template_rows = vec![length(200.0)];
        let root = tree.new_with_children(root_style, &[item]).unwrap();
        tree.compute_layout_with_measure(root, Size::MAX_CONTENT, measure_replaced_child).unwrap();
        let layout = tree.layout(item).unwrap();
        (layout.location, layout.size)
    }

    fn inline_margins(left_auto: bool, right_auto: bool) -> Rect<LengthPercentageAuto> {
        Rect {
            left: if left_auto { auto() } else { zero() },
            right: if right_auto { auto() } else { zero() },
            top: zero(),
            bottom: zero(),
        }
    }

    fn layout_nested_grid_item(
        intrinsic: IntrinsicInlineSize,
        margin: Rect<LengthPercentageAuto>,
        justify_self: Option<AlignSelf>,
        min_width: Dimension,
        max_width: Dimension,
        padding: Rect<LengthPercentage>,
        border: Rect<LengthPercentage>,
        box_sizing: BoxSizing,
    ) -> f32 {
        let mut tree = TaffyTree::new();
        tree.disable_rounding();

        let text = tree.new_leaf_with_context(Style::default(), intrinsic).unwrap();
        let item = tree
            .new_with_children(
                Style {
                    display: Display::Grid,
                    grid_template_columns: vec![fr(1.0)],
                    margin,
                    justify_self,
                    min_size: Size { width: min_width, height: Dimension::auto() },
                    max_size: Size { width: max_width, height: Dimension::auto() },
                    padding,
                    border,
                    box_sizing,
                    ..Style::default()
                },
                &[text],
            )
            .unwrap();
        let root = tree
            .new_with_children(
                Style {
                    display: Display::Grid,
                    size: Size { width: length(300.0), height: auto() },
                    grid_template_columns: vec![length(300.0)],
                    ..Style::default()
                },
                &[item],
            )
            .unwrap();

        tree.compute_layout_with_measure(root, Size::MAX_CONTENT, measure_intrinsic_child).unwrap();
        tree.layout(item).unwrap().size.width
    }

    fn unconstrained_item_width(
        intrinsic: IntrinsicInlineSize,
        margin: Rect<LengthPercentageAuto>,
        justify_self: Option<AlignSelf>,
    ) -> f32 {
        layout_nested_grid_item(
            intrinsic,
            margin,
            justify_self,
            Dimension::auto(),
            Dimension::auto(),
            Rect::zero(),
            Rect::zero(),
            BoxSizing::BorderBox,
        )
    }

    #[test]
    fn breakable_fit_content_honors_auto_margin_and_self_alignment_matrix() {
        let breakable = IntrinsicInlineSize { min: 100.0, max: 600.0 };
        let cases = [
            (inline_margins(true, true), None),
            (inline_margins(true, false), None),
            (inline_margins(false, true), None),
            (inline_margins(true, true), Some(AlignSelf::STRETCH)),
            (inline_margins(false, false), Some(AlignSelf::START)),
            (inline_margins(false, false), Some(AlignSelf::CENTER)),
            (inline_margins(false, false), Some(AlignSelf::END)),
        ];

        for (margin, justify_self) in cases {
            assert_eq!(unconstrained_item_width(breakable, margin, justify_self), 300.0);
        }
    }

    #[test]
    fn unbreakable_fit_content_preserves_the_min_content_floor() {
        let unbreakable = IntrinsicInlineSize { min: 600.0, max: 600.0 };
        let cases = [
            (inline_margins(true, true), None),
            (inline_margins(true, false), None),
            (inline_margins(false, true), None),
            (inline_margins(true, true), Some(AlignSelf::STRETCH)),
            (inline_margins(false, false), Some(AlignSelf::START)),
            (inline_margins(false, false), Some(AlignSelf::CENTER)),
            (inline_margins(false, false), Some(AlignSelf::END)),
        ];

        for (margin, justify_self) in cases {
            assert_eq!(unconstrained_item_width(unbreakable, margin, justify_self), 600.0);
        }

        assert_eq!(
            unconstrained_item_width(unbreakable, inline_margins(false, false), None),
            300.0,
            "stretch remains active when neither inline margin is auto"
        );
    }

    #[test]
    fn fit_content_applies_author_min_max_and_box_edges() {
        let breakable = IntrinsicInlineSize { min: 100.0, max: 600.0 };
        let unbreakable = IntrinsicInlineSize { min: 600.0, max: 600.0 };
        let auto_margins = inline_margins(true, true);

        let author_min = layout_nested_grid_item(
            breakable,
            auto_margins,
            None,
            length(350.0),
            Dimension::auto(),
            Rect::zero(),
            Rect::zero(),
            BoxSizing::BorderBox,
        );
        assert_eq!(author_min, 350.0);

        let author_max = layout_nested_grid_item(
            unbreakable,
            auto_margins,
            None,
            Dimension::auto(),
            length(250.0),
            Rect::zero(),
            Rect::zero(),
            BoxSizing::BorderBox,
        );
        assert_eq!(author_max, 250.0);

        let edges = Rect { left: length(10.0), right: length(10.0), top: zero(), bottom: zero() };
        let content_box_max = layout_nested_grid_item(
            unbreakable,
            auto_margins,
            None,
            Dimension::auto(),
            length(250.0),
            edges,
            edges,
            BoxSizing::ContentBox,
        );
        assert_eq!(content_box_max, 290.0);
    }

    #[test]
    fn grid_normal_uses_natural_replaced_size_but_stretches_ordinary_items() {
        let (_, replaced) = layout_replaced_grid_item(
            Style { item_is_replaced: true, ..Style::default() },
            Style::default(),
        );
        assert_eq!(replaced, Size { width: 100.0, height: 50.0 });

        let (_, ordinary) = layout_replaced_grid_item(Style::default(), Style::default());
        assert_eq!(ordinary, Size { width: 300.0, height: 150.0 });
    }

    #[test]
    fn ordinary_grid_aspect_ratio_preserves_normal_alignment_provenance() {
        let cases = [
            (None, None, None, None, Size { width: 300.0, height: 150.0 }),
            (None, Some(AlignSelf::STRETCH), None, None, Size { width: 400.0, height: 200.0 }),
            (Some(AlignSelf::STRETCH), None, None, None, Size { width: 300.0, height: 150.0 }),
            (
                Some(AlignSelf::STRETCH),
                Some(AlignSelf::STRETCH),
                None,
                None,
                Size { width: 300.0, height: 200.0 },
            ),
            (None, Some(AlignSelf::START), None, None, Size { width: 300.0, height: 150.0 }),
            (Some(AlignSelf::START), None, None, None, Size { width: 400.0, height: 200.0 }),
            (None, None, Some(AlignItems::STRETCH), None, Size { width: 400.0, height: 200.0 }),
            (None, None, None, Some(AlignItems::STRETCH), Size { width: 300.0, height: 150.0 }),
        ];
        for (justify_self, align_self, align_items, justify_items, expected) in cases {
            let (_, actual) = layout_replaced_grid_item(
                Style { justify_self, align_self, ..Style::default() },
                Style { align_items, justify_items, ..Style::default() },
            );
            assert_eq!(actual, expected, "justify={justify_self:?}, align={align_self:?}");
        }
    }

    #[test]
    fn replaced_grid_normal_applies_percentage_minimums_without_implicit_stretch() {
        let (_, inline_min) = layout_replaced_grid_item(
            Style {
                item_is_replaced: true,
                min_size: Size { width: percent(0.5), height: Dimension::auto() },
                ..Style::default()
            },
            Style::default(),
        );
        assert_eq!(inline_min, Size { width: 150.0, height: 75.0 });

        let (_, block_min) = layout_replaced_grid_item(
            Style {
                item_is_replaced: true,
                min_size: Size { width: Dimension::auto(), height: percent(0.5) },
                ..Style::default()
            },
            Style::default(),
        );
        assert_eq!(block_min, Size { width: 200.0, height: 100.0 });
    }

    #[test]
    fn replaced_grid_explicit_stretch_precedes_aspect_ratio_transfer() {
        let base = Style { item_is_replaced: true, ..Style::default() };

        let (_, inline_stretch) = layout_replaced_grid_item(
            Style { justify_self: Some(AlignSelf::STRETCH), ..base.clone() },
            Style::default(),
        );
        assert_eq!(inline_stretch, Size { width: 300.0, height: 150.0 });

        let (_, block_stretch) = layout_replaced_grid_item(
            Style { align_self: Some(AlignSelf::STRETCH), ..base.clone() },
            Style::default(),
        );
        assert_eq!(block_stretch, Size { width: 400.0, height: 200.0 });

        let (_, both_stretch) = layout_replaced_grid_item(
            Style {
                justify_self: Some(AlignSelf::STRETCH),
                align_self: Some(AlignSelf::STRETCH),
                ..base.clone()
            },
            Style::default(),
        );
        assert_eq!(both_stretch, Size { width: 300.0, height: 200.0 });

        let (_, definite_inline) = layout_replaced_grid_item(
            Style {
                size: Size { width: length(120.0), height: Dimension::auto() },
                align_self: Some(AlignSelf::STRETCH),
                ..base.clone()
            },
            Style::default(),
        );
        assert_eq!(definite_inline, Size { width: 120.0, height: 200.0 });

        let (_, definite_block) = layout_replaced_grid_item(
            Style {
                size: Size { width: Dimension::auto(), height: length(80.0) },
                justify_self: Some(AlignSelf::STRETCH),
                ..base
            },
            Style::default(),
        );
        assert_eq!(definite_block, Size { width: 300.0, height: 80.0 });
    }

    #[test]
    fn replaced_grid_normal_and_auto_margins_remain_natural() {
        let (location, size) = layout_replaced_grid_item(
            Style {
                item_is_replaced: true,
                min_size: Size { width: percent(0.5), height: Dimension::auto() },
                margin: Rect { left: auto(), right: zero(), top: zero(), bottom: zero() },
                ..Style::default()
            },
            Style { justify_items: Some(AlignItems::NORMAL), ..Style::default() },
        );
        assert_eq!(size, Size { width: 150.0, height: 75.0 });
        assert_eq!(location.x, 150.0);
    }

    #[test]
    fn overflowing_auto_margins_override_unsafe_self_alignment() {
        let area = Line { start: 0.0, end: 300.0 };
        let oversized = 600.0;
        let margin_cases = [
            Line { start: None, end: Some(0.0) },
            Line { start: Some(0.0), end: None },
            Line { start: None, end: None },
        ];

        for alignment in [AlignSelf::CENTER, AlignSelf::END] {
            for margin in margin_cases {
                let (ltr_start, ltr_margin) = super::align_item_within_area(
                    area,
                    alignment,
                    oversized,
                    Position::Relative,
                    Line { start: None, end: None },
                    margin,
                    0.0,
                    crate::Direction::Ltr,
                );
                assert_eq!(ltr_start, 0.0);
                assert_eq!(ltr_margin, Line { start: 0.0, end: 0.0 });

                let (rtl_start, rtl_margin) = super::align_item_within_area(
                    area,
                    alignment,
                    oversized,
                    Position::Relative,
                    Line { start: None, end: None },
                    margin,
                    0.0,
                    crate::Direction::Rtl,
                );
                assert_eq!(rtl_start, -300.0);
                assert_eq!(rtl_margin, Line { start: 0.0, end: 0.0 });
            }
        }

        let (absolute_center, _) = super::align_item_within_area(
            area,
            AlignSelf::CENTER,
            oversized,
            Position::Absolute,
            Line { start: None, end: None },
            Line { start: None, end: Some(0.0) },
            0.0,
            crate::Direction::Ltr,
        );
        assert_eq!(absolute_center, -150.0);

        let (absolute_end, _) = super::align_item_within_area(
            area,
            AlignSelf::END,
            oversized,
            Position::Absolute,
            Line { start: None, end: None },
            Line { start: None, end: Some(0.0) },
            0.0,
            crate::Direction::Ltr,
        );
        assert_eq!(absolute_end, -300.0);
    }
}
