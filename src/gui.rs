//! Retained Radiant editor for the beat-synced waveform viewer.

use std::sync::Arc;

use radiant::application::{
    DropdownOption, IntoView, dropdown_menu_overlay_below, dropdown_trigger,
};
use radiant::gui::automation::{AutomationLiveRegion, AutomationRole};
use radiant::gui::types::{Point, Rect, Rgba8, Vector2};
use radiant::layout::{CrossAlign, LayoutOutput, MainAlign};
use radiant::prelude::{
    TextAlign, TextColorRole, ViewNode, column, custom_widget_direct, custom_widget_mapped,
    pointer_shield, row, spacer, stack, text,
};
use radiant::runtime::{
    DeclarativeSurfaceRuntime, Event, PaintFillPolygon, PaintFillRect, PaintFillRectBatch,
    PaintPrimitive, PaintStrokePolyline, PaintStrokeRect, PaintText, PaintTextAlign, PaintTextRun,
    SurfacePaintPlan, UiSurface,
};
use radiant::theme::ThemeTokens;
use radiant::widgets::{
    ButtonMessage, ButtonWidget, PointerButton, PointerShieldMessage, Widget, WidgetCapabilities,
    WidgetCommon, WidgetInput, WidgetKey, WidgetOutput, WidgetSemantics, WidgetSemanticsRevision,
    WidgetSizing,
};

use crate::capture::{
    DEFAULT_SAMPLE_RATE, ENVELOPE_BINS, EnvelopePoint, WaveformPublication, WaveformView,
    WindowLength, current_phase, grid_x, window_phase,
};

/// Preferred logical width of the embedded editor.
pub const WINDOW_WIDTH: u32 = 960;
/// Preferred logical height of the embedded editor.
pub const WINDOW_HEIGHT: u32 = 600;

const WINDOW_DROPDOWN_WIDTH: f32 = 236.0;
const WINDOW_HEADER_HEIGHT: f32 = 45.9;
const WINDOW_HEADER_CONTROL_HEIGHT: f32 = 34.0;
const WINDOW_DROPDOWN_TRIGGER_HEIGHT: f32 = 24.0;
const WINDOW_DROPDOWN_TRIGGER_Y: f32 = (WINDOW_HEADER_HEIGHT - WINDOW_HEADER_CONTROL_HEIGHT) * 0.5
    + (WINDOW_HEADER_CONTROL_HEIGHT - WINDOW_DROPDOWN_TRIGGER_HEIGHT) * 0.5;
const WINDOW_DROPDOWN_GAP: f32 = 4.0;
const WINDOW_DROPDOWN_MENU_WIDTH: f32 = 260.0;
const WINDOW_HEADER_LABEL_WIDTH: f32 = 52.0;
const WINDOW_HEADER_CONTROL_GAP: f32 = 4.0;
const WINDOW_DROPDOWN_X: f32 = WINDOW_HEADER_LABEL_WIDTH + WINDOW_HEADER_CONTROL_GAP;
const WINDOW_HEADER_BRAND_WIDTH: f32 = 153.0;
const WINDOW_HEADER_BRAND_WORDMARK_WIDTH: f32 = 98.0;
const WINDOW_HEADER_BRAND_TITLE_HEIGHT: f32 = 27.2;
const WINDOW_HEADER_BRAND_META_HEIGHT: f32 = 13.6;
const WINDOW_HEADER_BRAND_HEIGHT: f32 = WINDOW_HEADER_HEIGHT;
const WINDOW_HELP_BUTTON_SIZE: f32 = 28.0;
const WINDOW_HELP_WIDTH: f32 = 360.0;
const WINDOW_HELP_HEIGHT: f32 = 160.0;
const WINDOW_HELP_RIGHT_INSET: f32 = 16.0;
const WINDOW_HEADER_BUTTON_TEXT_TOP_INSET: f32 = 3.4;
const WINDOW_HEADER_BUTTON_FONT_SIZE: f32 = 12.0;
const WINDOW_DROPDOWN_WIDGET_ID: u64 = 0x5741_5645_0000_0001;
const WINDOW_HELP_BUTTON_WIDGET_ID: u64 = 0x5741_5645_0000_0002;
const WAVEFORM_OFFSET_READOUT_WIDGET_ID: u64 = 0x5741_5645_0000_0003;
const WAVEFORM_WIDGET_ID: u64 = 0x5741_5645_0000_0004;
const WINDOW_VERSION_LABEL: &str = env!("CARGO_PKG_VERSION");

const WINDOW_HELP_ROWS: [(&str, &str); 6] = [
    ("Tab / Shift + Tab", "Move focus"),
    ("Enter / Space", "Activate the focused control"),
    ("WINDOW menu", "Select a beat window"),
    ("Command + Shift + double-click", "Reset waveform offset"),
    ("Escape", "Dismiss the open menu or help"),
    ("Click outside", "Dismiss the open menu or help"),
];

#[derive(Clone)]
struct WaveformWidget {
    common: WidgetCommon,
    view: WaveformView,
    waveform_offset: f32,
    command_held: bool,
    shift_held: bool,
    active_offset: Option<WaveformOffsetDrag>,
    chart_hovered: bool,
    retained_envelope: [EnvelopePoint; ENVELOPE_BINS],
    retained_waveform_bins: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatusPresentation {
    Live(WindowLength),
    Held(WindowLength),
    Waiting,
}

impl WaveformWidget {
    #[cfg(test)]
    fn new(view: WaveformView) -> Self {
        Self::new_with_interaction(view, 0.0, false, false, None)
    }

    fn new_with_interaction(
        view: WaveformView,
        waveform_offset: f32,
        command_held: bool,
        shift_held: bool,
        active_offset: Option<WaveformOffsetDrag>,
    ) -> Self {
        let (retained_waveform_bins, retained_envelope) = waveform_source_for_view(view)
            .unwrap_or((0, [EnvelopePoint::default(); ENVELOPE_BINS]));
        Self {
            common: WidgetCommon::new(
                1,
                WidgetSizing::new(Vector2::new(1.0, 1.0), Vector2::new(720.0, 420.0)),
            )
            .without_default_chrome(),
            view,
            waveform_offset: normalize_waveform_offset(waveform_offset),
            command_held,
            shift_held,
            active_offset,
            chart_hovered: false,
            retained_envelope,
            retained_waveform_bins,
        }
    }

    fn chart_rect(bounds: Rect) -> Rect {
        let width = bounds.width().max(1.0);
        let height = bounds.height().max(1.0);
        let header_height = 34.0_f32.min(height * 0.2);
        let footer_height = 28.0_f32.min(height * 0.12);
        Rect::from_xy_size(
            bounds.min.x + 16.0,
            bounds.min.y + header_height,
            (width - 32.0).max(1.0),
            (height - header_height - footer_height - 16.0).max(1.0),
        )
    }

    fn offset_hovered(&self) -> bool {
        self.chart_hovered && self.command_held && self.shift_held
    }

    fn pointer_offset(bounds: Rect, position: Point) -> f32 {
        let chart = Self::chart_rect(bounds);
        (position.x - chart.min.x) / chart.width().max(1.0)
    }

    fn waveform_for_paint(&self) -> (usize, [EnvelopePoint; ENVELOPE_BINS]) {
        let mut envelope = self.retained_envelope;
        let mut drawn_bins = self.retained_waveform_bins;
        if let Some((current_bins, current_envelope)) = waveform_source_for_view(self.view) {
            envelope[..current_bins].copy_from_slice(&current_envelope[..current_bins]);
            drawn_bins = drawn_bins.max(current_bins);
        }
        (drawn_bins, envelope)
    }

    fn status_text(&self) -> String {
        match self.status_presentation() {
            StatusPresentation::Live(window) => {
                format!("LIVE · {}", concise_window_label(window))
            }
            StatusPresentation::Held(window) => {
                format!("HELD · {}", concise_window_label(window))
            }
            StatusPresentation::Waiting => "WAITING".to_owned(),
        }
    }

    fn showing_live_preview(&self) -> bool {
        self.view.is_playing
            && self.view.live_valid
            && self.view.live_sample_count > 0
            && self.view.target_sample_count > 0
    }

    fn showing_retained_preview(&self) -> bool {
        !self.showing_live_preview()
            && self.view.display_valid
            && self.view.display_sample_count > 0
            && self.view.display_target_sample_count > 0
    }

    fn displayed_window(&self) -> WindowLength {
        if self.showing_live_preview() {
            self.view.live_window
        } else if self.showing_retained_preview() {
            self.view.display_window
        } else {
            self.view.snapshot_window
        }
    }

    fn status_presentation(&self) -> StatusPresentation {
        if self.showing_live_preview() {
            StatusPresentation::Live(self.view.live_window)
        } else if self.showing_retained_preview()
            || (self.view.snapshot_revision > 0 && self.view.sample_count > 0)
        {
            StatusPresentation::Held(self.displayed_window())
        } else {
            StatusPresentation::Waiting
        }
    }
}

fn concise_window_label(window: WindowLength) -> &'static str {
    match window {
        WindowLength::OneBeat => "1 beat",
        WindowLength::TwoBeats => "2 beats",
        WindowLength::FourBeats => "4 beats",
        WindowLength::EightBeats => "8 beats",
    }
}

impl WidgetSemantics for WaveformWidget {
    fn revision(&self) -> WidgetSemanticsRevision {
        WidgetSemanticsRevision::exact(self.status_presentation())
    }

    fn automation_role(&self) -> AutomationRole {
        AutomationRole::Readout
    }

    fn automation_label(&self) -> Option<String> {
        Some("Capture status".to_owned())
    }

    fn automation_value_text(&self) -> Option<String> {
        Some(self.status_text())
    }

    fn automation_live_region(&self) -> AutomationLiveRegion {
        AutomationLiveRegion::Polite
    }
}

impl Widget for WaveformWidget {
    fn common(&self) -> &WidgetCommon {
        &self.common
    }

    fn common_mut(&mut self) -> &mut WidgetCommon {
        &mut self.common
    }

    fn capabilities(&self) -> WidgetCapabilities<'_> {
        WidgetCapabilities::new().semantics(self)
    }

    fn handle_input(&mut self, bounds: Rect, input: WidgetInput) -> Option<WidgetOutput> {
        let message = match input {
            WidgetInput::PointerMove { position } => {
                self.common.state.hovered = bounds.contains(position);
                self.chart_hovered = Self::chart_rect(bounds).contains(position);
                self.active_offset
                    .map(|drag| EditorMessage::DragWaveformOffset {
                        delta: Self::pointer_offset(bounds, position) - drag.start_pointer_x,
                    })
            }
            WidgetInput::PointerModifiersChanged { modifiers } => {
                (self.command_held != modifiers.command || self.shift_held != modifiers.shift)
                    .then_some(EditorMessage::SetWaveformModifiers {
                        command_held: modifiers.command,
                        shift_held: modifiers.shift,
                    })
            }
            WidgetInput::PointerDoubleClick {
                position,
                button: PointerButton::Primary,
                modifiers,
            } if Self::chart_rect(bounds).contains(position)
                && modifiers.command
                && modifiers.shift
                && !modifiers.alt =>
            {
                self.common.state.hovered = true;
                self.common.state.pressed = false;
                self.chart_hovered = true;
                self.active_offset = None;
                Some(EditorMessage::ResetWaveformOffset)
            }
            WidgetInput::PointerPress {
                position,
                button: PointerButton::Primary,
                modifiers,
            } if Self::chart_rect(bounds).contains(position)
                && modifiers.command
                && modifiers.shift
                && !modifiers.alt =>
            {
                self.common.state.hovered = true;
                self.common.state.pressed = true;
                self.chart_hovered = true;
                Some(EditorMessage::BeginWaveformOffset {
                    pointer_x: Self::pointer_offset(bounds, position),
                })
            }
            WidgetInput::PointerRelease {
                position,
                button: PointerButton::Primary,
                ..
            }
            | WidgetInput::PointerDrop {
                position,
                button: PointerButton::Primary,
                ..
            } => {
                self.common.state.pressed = false;
                self.common.state.hovered = bounds.contains(position);
                self.chart_hovered = Self::chart_rect(bounds).contains(position);
                self.active_offset
                    .map(|drag| EditorMessage::EndWaveformOffset {
                        delta: Self::pointer_offset(bounds, position) - drag.start_pointer_x,
                    })
            }
            WidgetInput::FocusChanged(focused) => {
                self.common.state.focused = focused;
                (!focused && self.active_offset.is_some())
                    .then_some(EditorMessage::CancelWaveformOffset)
            }
            _ => None,
        }?;
        Some(WidgetOutput::typed(message))
    }

    fn synchronize_from_previous(&mut self, previous: &dyn Widget) {
        let Some(previous) = previous.as_any().downcast_ref::<Self>() else {
            return;
        };
        self.common.state = previous.common.state;
        self.chart_hovered = previous.chart_hovered;
        let (retained_waveform_bins, retained_envelope) = previous.waveform_for_paint();
        if retained_waveform_bins > 0 {
            self.retained_waveform_bins = retained_waveform_bins;
            self.retained_envelope = retained_envelope;
        }
    }

    fn append_paint(
        &self,
        primitives: &mut Vec<PaintPrimitive>,
        bounds: Rect,
        _layout: &LayoutOutput,
        theme: &ThemeTokens,
    ) {
        let id = self.common.id;
        let width = bounds.width().max(1.0);
        let chart = Self::chart_rect(bounds);
        let offset_hovered = self.offset_hovered();
        let offset_active = self.active_offset.is_some();

        primitives.push(PaintPrimitive::FillRect(PaintFillRect {
            widget_id: id,
            rect: bounds,
            color: theme.bg_primary,
        }));
        primitives.push(PaintPrimitive::Text(PaintTextRun {
            widget_id: id,
            text: PaintText::from(self.status_text()),
            rect: Rect::from_xy_size(bounds.min.x + 16.0, bounds.min.y + 8.0, width - 32.0, 14.0),
            font_size: 11.0,
            baseline: Some(11.0),
            color: if self.showing_live_preview() {
                theme.highlight_blue_soft
            } else if self.showing_retained_preview() {
                theme.highlight_orange
            } else {
                theme.text_muted
            },
            align: PaintTextAlign::Left,
            wrap: radiant::widgets::TextWrap::None,
        }));

        primitives.push(PaintPrimitive::FillRect(PaintFillRect {
            widget_id: id,
            rect: chart,
            color: theme.surface_base,
        }));
        primitives.push(PaintPrimitive::StrokeRect(PaintStrokeRect {
            widget_id: id,
            rect: chart,
            color: if offset_active {
                theme.highlight_cyan.blend_toward(theme.text_primary, 0.55)
            } else if offset_hovered {
                theme.highlight_cyan
            } else {
                theme.border_emphasis
            },
            width: if offset_active { 2.0 } else { 1.0 },
        }));

        let center_y = chart.min.y + chart.height() * 0.5;
        primitives.push(PaintPrimitive::FillRect(PaintFillRect {
            widget_id: id,
            rect: Rect::from_xy_size(chart.min.x, center_y, chart.width(), 1.0),
            color: theme.grid_strong,
        }));

        let displayed_window = self.displayed_window();
        let window_beats = displayed_window.beats() as usize;
        for subdivision in 0..=window_beats.saturating_mul(4) {
            let beat_offset = subdivision as f64 * 0.25;
            let x = chart.min.x
                + grid_x(
                    beat_offset,
                    f64::from(displayed_window.beats()),
                    chart.width(),
                );
            primitives.push(PaintPrimitive::FillRect(PaintFillRect {
                widget_id: id,
                rect: Rect::from_xy_size(x, chart.min.y, 1.0, chart.height()),
                color: if subdivision % 4 == 0 {
                    theme.border_emphasis
                } else {
                    theme.grid_soft
                },
            }));
        }

        let show_live = self.showing_live_preview();
        let show_retained = self.showing_retained_preview();
        let (drawn_bins, display_envelope) = self.waveform_for_paint();
        if drawn_bins > 0 {
            let mut peak_rects = Vec::new();
            let mut source_points = Vec::with_capacity(drawn_bins);
            for (index, point) in display_envelope.iter().take(drawn_bins).enumerate() {
                let min = point.min.clamp(-1.0, 1.0);
                let max = point.max.clamp(-1.0, 1.0);
                let source_x = index as f32 / ENVELOPE_BINS.saturating_sub(1).max(1) as f32;
                let x = chart.min.x
                    + shifted_waveform_x(source_x, self.waveform_offset) * chart.width();
                let (top, bottom) = waveform_y_points(*point, chart);
                source_points.push(*point);
                if max.abs().max(min.abs()) > 0.85 {
                    peak_rects.push(Rect::from_xy_size(
                        x.clamp(chart.min.x, chart.max.x) - 0.75,
                        top,
                        1.5,
                        (bottom - top).max(1.0),
                    ));
                }
            }

            let waveform_color = if show_live {
                theme.highlight_blue
            } else if show_retained {
                theme.highlight_orange
            } else {
                theme.accent_mint
            };
            let waveform_color = if offset_active {
                theme.highlight_cyan.blend_toward(theme.text_primary, 0.55)
            } else if offset_hovered {
                theme.highlight_cyan
            } else {
                waveform_color
            };
            for segment in shifted_waveform_segments(&source_points, chart, self.waveform_offset) {
                let mut fill_points = Vec::with_capacity(segment.top.len() + segment.bottom.len());
                fill_points.extend(segment.top.iter().copied());
                fill_points.extend(segment.bottom.iter().rev().copied());
                primitives.push(PaintPrimitive::FillPolygon(PaintFillPolygon {
                    widget_id: id,
                    points: fill_points.into(),
                    color: waveform_color.with_alpha(if show_live { 32 } else { 64 }),
                }));
                primitives.push(PaintPrimitive::StrokePolyline(PaintStrokePolyline {
                    widget_id: id,
                    points: segment.top.into(),
                    color: waveform_color.with_alpha(if show_live { 176 } else { 232 }),
                    width: if offset_active { 1.5 } else { 1.0 },
                }));
                primitives.push(PaintPrimitive::StrokePolyline(PaintStrokePolyline {
                    widget_id: id,
                    points: segment.bottom.into(),
                    color: waveform_color.with_alpha(if show_live { 128 } else { 184 }),
                    width: if offset_active { 1.5 } else { 1.0 },
                }));
            }
            if !peak_rects.is_empty() {
                primitives.push(PaintPrimitive::FillRectBatch(PaintFillRectBatch {
                    widget_id: id,
                    rects: peak_rects.into(),
                    color: if offset_active {
                        theme
                            .highlight_cyan
                            .blend_toward(theme.text_primary, 0.55)
                            .with_alpha(220)
                    } else if offset_hovered {
                        theme.highlight_cyan.with_alpha(220)
                    } else if show_live {
                        theme.highlight_blue.with_alpha(144)
                    } else {
                        theme.highlight_orange.with_alpha(220)
                    },
                }));
            }
        }

        let phase = if show_retained {
            None
        } else if displayed_window == WindowLength::OneBeat {
            current_phase(self.view.current_song_pos_beats)
        } else {
            window_phase(self.view.current_song_pos_beats, displayed_window)
        };
        if let Some(phase) = phase {
            let x = chart.min.x + phase * chart.width();
            primitives.push(PaintPrimitive::FillRect(PaintFillRect {
                widget_id: id,
                rect: Rect::from_xy_size(x - 1.0, chart.min.y, 2.0, chart.height()),
                color: theme.highlight_orange,
            }));
        }
        primitives.push(PaintPrimitive::FillRect(PaintFillRect {
            widget_id: id,
            rect: zero_marker_rect(chart),
            color: theme.text_primary.with_alpha(192),
        }));
    }
}

fn captured_prefix_bins(sample_count: usize, target_sample_count: usize) -> usize {
    if sample_count == 0 || target_sample_count == 0 {
        return 0;
    }
    if sample_count >= target_sample_count {
        return ENVELOPE_BINS;
    }
    ((sample_count as u128 * ENVELOPE_BINS as u128).div_ceil(target_sample_count as u128) as usize)
        .clamp(1, ENVELOPE_BINS)
}

fn waveform_source_for_view(view: WaveformView) -> Option<(usize, [EnvelopePoint; ENVELOPE_BINS])> {
    let show_live = view.is_playing
        && view.live_valid
        && view.live_sample_count > 0
        && view.target_sample_count > 0;
    let show_retained = !show_live
        && view.display_valid
        && view.display_sample_count > 0
        && view.display_target_sample_count > 0;
    let (sample_count, target_sample_count, source) = if show_live {
        (
            view.live_sample_count,
            view.target_sample_count,
            view.live_envelope,
        )
    } else if show_retained {
        (
            view.display_sample_count,
            view.display_target_sample_count,
            view.display_envelope,
        )
    } else if view.sample_count > 0 {
        (view.sample_count, view.sample_count, view.envelope)
    } else {
        return None;
    };
    let captured_bins = captured_prefix_bins(sample_count, target_sample_count);
    if captured_bins == 0 {
        return None;
    }

    let preserve_completed_tail =
        (show_live || show_retained) && view.snapshot_revision > 0 && view.sample_count > 0;
    let drawn_bins = if preserve_completed_tail {
        ENVELOPE_BINS
    } else {
        captured_bins
    };
    let mut envelope = if preserve_completed_tail {
        view.envelope
    } else {
        [EnvelopePoint::default(); ENVELOPE_BINS]
    };
    envelope[..captured_bins].copy_from_slice(&source[..captured_bins]);
    Some((drawn_bins, envelope))
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WaveformOffsetDrag {
    origin_offset: f32,
    start_pointer_x: f32,
}

#[derive(Default)]
struct WaveformSegment {
    top: Vec<Point>,
    bottom: Vec<Point>,
}

fn normalize_waveform_offset(offset: f32) -> f32 {
    if offset.is_finite() {
        offset.rem_euclid(1.0)
    } else {
        0.0
    }
}

fn signed_waveform_offset(offset: f32) -> f64 {
    let normalized = f64::from(normalize_waveform_offset(offset));
    if normalized > 0.5 {
        normalized - 1.0
    } else {
        normalized
    }
}

fn shifted_waveform_x(source_x: f32, offset: f32) -> f32 {
    let translated_x = source_x + normalize_waveform_offset(offset);
    if translated_x <= 1.0 {
        translated_x
    } else {
        translated_x.rem_euclid(1.0)
    }
}

fn waveform_y_points(point: EnvelopePoint, chart: Rect) -> (f32, f32) {
    let min = point.min.clamp(-1.0, 1.0);
    let max = point.max.clamp(-1.0, 1.0);
    (
        chart.min.y + (1.0 - max) * chart.height() * 0.5,
        chart.min.y + (1.0 - min) * chart.height() * 0.5,
    )
}

fn shifted_waveform_segments(
    source: &[EnvelopePoint],
    chart: Rect,
    offset: f32,
) -> Vec<WaveformSegment> {
    if source.is_empty() {
        return Vec::new();
    }

    let denominator = ENVELOPE_BINS.saturating_sub(1).max(1) as f32;
    let offset = normalize_waveform_offset(offset);
    let mut segments = vec![WaveformSegment::default()];
    let mut previous_translated_x = offset;
    let (mut previous_top_y, mut previous_bottom_y) = waveform_y_points(source[0], chart);
    segments[0].top.push(Point::new(
        chart.min.x + previous_translated_x * chart.width(),
        previous_top_y,
    ));
    segments[0].bottom.push(Point::new(
        chart.min.x + previous_translated_x * chart.width(),
        previous_bottom_y,
    ));

    for (index, point) in source.iter().enumerate().skip(1) {
        let source_x = index as f32 / denominator;
        let translated_x = source_x + offset;
        let (top_y, bottom_y) = waveform_y_points(*point, chart);
        if translated_x <= 1.0 {
            segments.last_mut().unwrap().top.push(Point::new(
                chart.min.x + translated_x * chart.width(),
                top_y,
            ));
            segments.last_mut().unwrap().bottom.push(Point::new(
                chart.min.x + translated_x * chart.width(),
                bottom_y,
            ));
        } else if previous_translated_x <= 1.0 {
            let fraction = ((1.0 - previous_translated_x) / (translated_x - previous_translated_x))
                .clamp(0.0, 1.0);
            let seam_top_y = previous_top_y + (top_y - previous_top_y) * fraction;
            let seam_bottom_y = previous_bottom_y + (bottom_y - previous_bottom_y) * fraction;
            let current = segments.last_mut().unwrap();
            current.top.push(Point::new(chart.max.x, seam_top_y));
            current.bottom.push(Point::new(chart.max.x, seam_bottom_y));

            let wrapped_x = translated_x - 1.0;
            let mut wrapped = WaveformSegment::default();
            wrapped.top.push(Point::new(chart.min.x, seam_top_y));
            wrapped.bottom.push(Point::new(chart.min.x, seam_bottom_y));
            wrapped
                .top
                .push(Point::new(chart.min.x + wrapped_x * chart.width(), top_y));
            wrapped.bottom.push(Point::new(
                chart.min.x + wrapped_x * chart.width(),
                bottom_y,
            ));
            segments.push(wrapped);
        } else {
            let wrapped_x = translated_x - 1.0;
            segments
                .last_mut()
                .unwrap()
                .top
                .push(Point::new(chart.min.x + wrapped_x * chart.width(), top_y));
            segments.last_mut().unwrap().bottom.push(Point::new(
                chart.min.x + wrapped_x * chart.width(),
                bottom_y,
            ));
        }
        previous_translated_x = translated_x;
        previous_top_y = top_y;
        previous_bottom_y = bottom_y;
    }

    segments
}

fn offset_sample_count(view: WaveformView) -> usize {
    if view.is_playing
        && view.live_valid
        && view.live_sample_count > 0
        && view.target_sample_count > 0
    {
        view.target_sample_count
    } else if view.display_valid
        && view.display_sample_count > 0
        && view.display_target_sample_count > 0
    {
        view.display_target_sample_count
    } else {
        view.sample_count
    }
}

fn format_waveform_offset(offset: f32, sample_count: usize, sample_rate: f64) -> String {
    let offset_samples = (signed_waveform_offset(offset) * sample_count as f64).round() as i64;
    if offset_samples == 0 {
        return "0 samples · 0.00 ms".to_owned();
    }

    let sample_rate = if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        DEFAULT_SAMPLE_RATE
    };
    let milliseconds = offset_samples as f64 / sample_rate * 1000.0;
    let rounded_milliseconds = (milliseconds.abs() * 100.0).round() / 100.0;
    let sign = if offset_samples.is_negative() {
        '-'
    } else {
        '+'
    };
    let milliseconds_text = if rounded_milliseconds == 0.0 {
        "0.00".to_owned()
    } else {
        format!("{sign}{rounded_milliseconds:.2}")
    };
    format!(
        "{sign}{offset_samples_abs} samples · {milliseconds_text} ms",
        offset_samples_abs = offset_samples.unsigned_abs(),
    )
}

fn zero_marker_rect(chart: Rect) -> Rect {
    Rect::from_xy_size(chart.min.x - 2.0, chart.max.y - 3.0, 4.0, 3.0)
}

/// Non-focusable signed offset status shown beneath the waveform chart.
#[derive(Clone)]
struct WaveformOffsetReadoutWidget {
    common: WidgetCommon,
    offset_text: String,
}

impl WaveformOffsetReadoutWidget {
    fn new(offset: f32, sample_count: usize, sample_rate: f64) -> Self {
        Self {
            common: WidgetCommon::new(
                WAVEFORM_OFFSET_READOUT_WIDGET_ID,
                WidgetSizing::new(Vector2::new(1.0, 1.0), Vector2::new(720.0, 420.0)),
            )
            .without_default_chrome(),
            offset_text: format_waveform_offset(offset, sample_count, sample_rate),
        }
    }
}

impl Widget for WaveformOffsetReadoutWidget {
    fn common(&self) -> &WidgetCommon {
        &self.common
    }

    fn common_mut(&mut self) -> &mut WidgetCommon {
        &mut self.common
    }

    fn handle_input(&mut self, _bounds: Rect, _input: WidgetInput) -> Option<WidgetOutput> {
        None
    }

    fn accepts_pointer_move(&self) -> bool {
        false
    }

    fn accepts_pointer_input(&self, _input: &WidgetInput) -> bool {
        false
    }

    fn capabilities(&self) -> WidgetCapabilities<'_> {
        WidgetCapabilities::new().semantics(self)
    }

    fn append_paint(
        &self,
        primitives: &mut Vec<PaintPrimitive>,
        bounds: Rect,
        _layout: &LayoutOutput,
        theme: &ThemeTokens,
    ) {
        let height = bounds.height().max(1.0);
        let footer_height = 28.0_f32.min(height * 0.12);
        primitives.push(PaintPrimitive::Text(PaintTextRun {
            widget_id: self.common.id,
            text: PaintText::from(self.offset_text.clone()),
            rect: Rect::from_xy_size(
                bounds.min.x + 16.0,
                bounds.max.y - footer_height + 3.0,
                (bounds.width() - 32.0).max(1.0),
                (footer_height - 3.0).max(1.0),
            ),
            font_size: 10.0,
            baseline: Some(10.0),
            color: theme.text_muted,
            align: PaintTextAlign::Left,
            wrap: radiant::widgets::TextWrap::None,
        }));
    }
}

impl WidgetSemantics for WaveformOffsetReadoutWidget {
    fn revision(&self) -> WidgetSemanticsRevision {
        WidgetSemanticsRevision::exact(self.offset_text.clone())
    }

    fn automation_role(&self) -> AutomationRole {
        AutomationRole::Readout
    }

    fn automation_label(&self) -> Option<String> {
        Some("Waveform offset".to_owned())
    }

    fn automation_description(&self) -> Option<String> {
        Some("Signed distance from the waveform true-zero position".to_owned())
    }

    fn automation_value_text(&self) -> Option<String> {
        Some(self.offset_text.clone())
    }

    fn automation_live_region(&self) -> AutomationLiveRegion {
        AutomationLiveRegion::None
    }
}

fn header_button_hover_fill(theme: &ThemeTokens) -> Rgba8 {
    theme
        .surface_base
        .blend_toward(theme.surface_overlay, theme.state_hover_strong)
}

fn header_button_text_rect(bounds: Rect) -> Rect {
    Rect::from_xy_size(
        bounds.min.x,
        bounds.min.y + WINDOW_HEADER_BUTTON_TEXT_TOP_INSET,
        bounds.width(),
        (bounds.height() - WINDOW_HEADER_BUTTON_TEXT_TOP_INSET).max(1.0),
    )
}

fn header_button_text_baseline(rect: Rect) -> f32 {
    (rect.height() * 0.5 + WINDOW_HEADER_BUTTON_FONT_SIZE * 0.35).max(0.0)
}

/// Compact question-mark control that opens the Wave help panel.
#[derive(Clone, Debug)]
struct WaveHelpButtonWidget {
    button: ButtonWidget,
}

impl WaveHelpButtonWidget {
    fn new() -> Self {
        Self {
            button: ButtonWidget::new(
                0,
                "?",
                WidgetSizing::fixed(Vector2::new(
                    WINDOW_HELP_BUTTON_SIZE,
                    WINDOW_HELP_BUTTON_SIZE,
                )),
            )
            .with_hover_chrome_only(),
        }
    }
}

impl Widget for WaveHelpButtonWidget {
    fn common(&self) -> &WidgetCommon {
        self.button.common()
    }

    fn common_mut(&mut self) -> &mut WidgetCommon {
        self.button.common_mut()
    }

    fn handle_input(&mut self, bounds: Rect, input: WidgetInput) -> Option<WidgetOutput> {
        self.button
            .handle_input(bounds, input)
            .map(WidgetOutput::typed)
    }

    fn accepts_pointer_move(&self) -> bool {
        true
    }

    fn synchronize_from_previous(&mut self, previous: &dyn Widget) {
        let Some(previous) = previous.as_any().downcast_ref::<Self>() else {
            return;
        };
        self.button.synchronize_from_previous(&previous.button);
    }

    fn capabilities(&self) -> WidgetCapabilities<'_> {
        WidgetCapabilities::new().semantics(self)
    }

    fn append_paint(
        &self,
        primitives: &mut Vec<PaintPrimitive>,
        bounds: Rect,
        _layout: &LayoutOutput,
        theme: &ThemeTokens,
    ) {
        let text_rect = header_button_text_rect(bounds);
        let fill = if self.button.common.state.pressed {
            theme.accent_copper.with_alpha(96)
        } else if self.button.common.state.hovered {
            header_button_hover_fill(theme)
        } else {
            theme.surface_base
        };
        primitives.push(PaintPrimitive::FillRect(PaintFillRect {
            widget_id: self.button.common.id,
            rect: bounds,
            color: fill,
        }));
        primitives.push(PaintPrimitive::StrokeRect(PaintStrokeRect {
            widget_id: self.button.common.id,
            rect: bounds,
            color: if self.button.common.state.focused {
                theme.accent_warning
            } else {
                theme.border_emphasis
            },
            width: 1.0,
        }));
        primitives.push(PaintPrimitive::Text(PaintTextRun {
            widget_id: self.button.common.id,
            text: PaintText::from_static("?"),
            rect: text_rect,
            font_size: WINDOW_HEADER_BUTTON_FONT_SIZE,
            baseline: Some(header_button_text_baseline(text_rect)),
            color: theme.text_primary,
            align: PaintTextAlign::Center,
            wrap: radiant::widgets::TextWrap::None,
        }));
    }
}

impl WidgetSemantics for WaveHelpButtonWidget {
    fn automation_role(&self) -> AutomationRole {
        AutomationRole::Button
    }

    fn automation_label(&self) -> Option<String> {
        Some("Show WAVE help".to_owned())
    }

    fn automation_description(&self) -> Option<String> {
        Some("Open the WAVE help panel".to_owned())
    }
}

/// Non-interactive Wave help content shown in a transient panel.
#[derive(Clone)]
struct WaveHelpWidget {
    common: WidgetCommon,
}

impl WaveHelpWidget {
    fn new() -> Self {
        Self {
            common: WidgetCommon::fixed(0, WINDOW_HELP_WIDTH, WINDOW_HELP_HEIGHT)
                .without_default_chrome(),
        }
    }
}

impl Widget for WaveHelpWidget {
    fn common(&self) -> &WidgetCommon {
        &self.common
    }

    fn common_mut(&mut self) -> &mut WidgetCommon {
        &mut self.common
    }

    fn handle_input(&mut self, _bounds: Rect, _input: WidgetInput) -> Option<WidgetOutput> {
        None
    }

    fn capabilities(&self) -> WidgetCapabilities<'_> {
        WidgetCapabilities::new().semantics(self)
    }

    fn append_paint(
        &self,
        primitives: &mut Vec<PaintPrimitive>,
        bounds: Rect,
        _layout: &LayoutOutput,
        theme: &ThemeTokens,
    ) {
        primitives.push(PaintPrimitive::FillRect(PaintFillRect {
            widget_id: self.common.id,
            rect: bounds.inset(1.0, 1.0, 1.0, 1.0),
            color: theme.surface_overlay,
        }));
        primitives.push(PaintPrimitive::StrokeRect(PaintStrokeRect {
            widget_id: self.common.id,
            rect: bounds,
            color: theme.border_emphasis,
            width: 1.0,
        }));
        primitives.push(PaintPrimitive::Text(PaintTextRun {
            widget_id: self.common.id,
            text: PaintText::from_static("WAVE HELP"),
            rect: Rect::from_xy_size(
                bounds.min.x + 13.6,
                bounds.min.y + 10.2,
                bounds.width() - 27.2,
                17.0,
            ),
            font_size: WINDOW_HEADER_BUTTON_FONT_SIZE,
            baseline: None,
            color: theme.accent_copper,
            align: PaintTextAlign::Left,
            wrap: radiant::widgets::TextWrap::None,
        }));

        let key_width = 190.0;
        let row_top = bounds.min.y + 35.7;
        for (index, (key, description)) in WINDOW_HELP_ROWS.into_iter().enumerate() {
            let y = row_top + index as f32 * 20.4;
            primitives.push(PaintPrimitive::Text(PaintTextRun {
                widget_id: self.common.id,
                text: PaintText::from_static(key),
                rect: Rect::from_xy_size(bounds.min.x + 13.6, y, key_width, 17.0),
                font_size: 10.0,
                baseline: None,
                color: theme.text_primary,
                align: PaintTextAlign::Left,
                wrap: radiant::widgets::TextWrap::None,
            }));
            primitives.push(PaintPrimitive::Text(PaintTextRun {
                widget_id: self.common.id,
                text: PaintText::from_static(description),
                rect: Rect::from_xy_size(
                    bounds.min.x + 13.6 + key_width,
                    y,
                    bounds.width() - key_width - 27.2,
                    17.0,
                ),
                font_size: 10.0,
                baseline: None,
                color: theme.text_muted,
                align: PaintTextAlign::Left,
                wrap: radiant::widgets::TextWrap::None,
            }));
        }
    }
}

impl WidgetSemantics for WaveHelpWidget {
    fn automation_role(&self) -> AutomationRole {
        AutomationRole::Text
    }

    fn automation_label(&self) -> Option<String> {
        Some("WAVE help".to_owned())
    }

    fn automation_description(&self) -> Option<String> {
        Some("Supported WAVE keyboard and pointer interactions".to_owned())
    }
}

struct EditorState {
    publication: Arc<WaveformPublication>,
    view: WaveformView,
    selected_window: WindowLength,
    window_dropdown_open: bool,
    help_open: bool,
    waveform_offset: f32,
    waveform_command_held: bool,
    waveform_shift_held: bool,
    active_waveform_offset: Option<WaveformOffsetDrag>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum EditorMessage {
    ToggleWindowDropdown,
    SelectWindow(WindowLength),
    ToggleHelp,
    DismissTransient,
    SetWaveformModifiers {
        command_held: bool,
        shift_held: bool,
    },
    BeginWaveformOffset {
        pointer_x: f32,
    },
    DragWaveformOffset {
        delta: f32,
    },
    EndWaveformOffset {
        delta: f32,
    },
    CancelWaveformOffset,
    ResetWaveformOffset,
}

type EditorRuntime = DeclarativeSurfaceRuntime<
    EditorState,
    EditorMessage,
    fn(&mut EditorState) -> Arc<UiSurface<EditorMessage>>,
    fn(&mut EditorState, EditorMessage),
>;

#[allow(clippy::arc_with_non_send_sync)]
fn project_surface(state: &mut EditorState) -> Arc<UiSurface<EditorMessage>> {
    let _ = state.publication.read_view(&mut state.view);
    state.selected_window = state.publication.selected_window();
    let window_options = WindowLength::ALL.into_iter().map(|window| {
        DropdownOption::new(
            window.label(),
            window == state.selected_window,
            EditorMessage::SelectWindow(window),
        )
    });
    let window_dropdown =
        dropdown_trigger(state.selected_window.label(), state.window_dropdown_open)
            .toggle_message(EditorMessage::ToggleWindowDropdown)
            .build()
            .key("window-dropdown")
            .id(WINDOW_DROPDOWN_WIDGET_ID)
            .width(WINDOW_DROPDOWN_WIDTH)
            .height(WINDOW_DROPDOWN_TRIGGER_HEIGHT);
    let window_controls = row([
        text("WINDOW")
            .key("window-label")
            .width(WINDOW_HEADER_LABEL_WIDTH),
        window_dropdown,
    ])
    .key("window-controls")
    .spacing(WINDOW_HEADER_CONTROL_GAP)
    .align_cross(CrossAlign::Center)
    .height(WINDOW_HEADER_CONTROL_HEIGHT);
    let header_brand = column([
        row([
            text("PortalSurfer")
                .muted_text()
                .width(WINDOW_HEADER_BRAND_WORDMARK_WIDTH)
                .align_text(TextAlign::Right),
            text("/")
                .muted_text()
                .width(8.5)
                .align_text(TextAlign::Center),
            text("WAVE")
                .text_color(TextColorRole::Custom(ThemeTokens::dark().accent_copper))
                .align_text(TextAlign::Right)
                .width(35.7),
        ])
        .key("wave-header-brand-title")
        .align_main(MainAlign::End)
        .fill_width()
        .height(WINDOW_HEADER_BRAND_TITLE_HEIGHT),
        text(WINDOW_VERSION_LABEL)
            .muted_text()
            .key("wave-header-version")
            .align_text(TextAlign::Right)
            .fill_width()
            .height(WINDOW_HEADER_BRAND_META_HEIGHT),
    ])
    .key("wave-header-brand")
    .spacing(0.0)
    .width(WINDOW_HEADER_BRAND_WIDTH)
    .height(WINDOW_HEADER_BRAND_HEIGHT);
    let help_action = custom_widget_mapped(WaveHelpButtonWidget::new(), move |_: ButtonMessage| {
        EditorMessage::ToggleHelp
    })
    .key("wave-help-button")
    .id(WINDOW_HELP_BUTTON_WIDGET_ID)
    .size(WINDOW_HELP_BUTTON_SIZE, WINDOW_HELP_BUTTON_SIZE)
    .tooltip("Show WAVE help");
    let header_row = row([
        window_controls,
        spacer().fill_width(),
        header_brand,
        help_action,
    ])
    .key("wave-header")
    .spacing(WINDOW_HEADER_CONTROL_GAP)
    .align_cross(CrossAlign::Center)
    .fill_width()
    .height(WINDOW_HEADER_HEIGHT);
    let editor = column([
        header_row,
        stack([
            custom_widget_direct(WaveformWidget::new_with_interaction(
                state.view,
                state.waveform_offset,
                state.waveform_command_held,
                state.waveform_shift_held,
                state.active_waveform_offset,
            ))
            .id(WAVEFORM_WIDGET_ID)
            .fill(),
            custom_widget_direct(WaveformOffsetReadoutWidget::new(
                state.waveform_offset,
                offset_sample_count(state.view),
                state.publication.sample_rate(),
            ))
            .key("waveform-offset-readout")
            .fill(),
        ])
        .key("waveform-layer")
        .fill(),
    ])
    .key("wave-editor")
    .fill();
    let surface = if state.window_dropdown_open {
        dismissible_wave_overlay(
            editor,
            dropdown_menu_overlay_below(
                WINDOW_DROPDOWN_X,
                WINDOW_DROPDOWN_TRIGGER_Y,
                WINDOW_DROPDOWN_TRIGGER_HEIGHT,
                WINDOW_DROPDOWN_GAP,
                Some(WINDOW_DROPDOWN_MENU_WIDTH),
                window_options.collect(),
            ),
        )
    } else if state.help_open {
        dismissible_wave_overlay(editor, wave_help_overlay())
    } else {
        stack([editor]).key("wave-surface-stack").fill()
    };
    Arc::new(surface.into_surface())
}

fn dismissible_wave_overlay(
    base: ViewNode<EditorMessage>,
    overlay: ViewNode<EditorMessage>,
) -> ViewNode<EditorMessage> {
    stack([
        base,
        pointer_shield(true)
            .filter_map(|message| match message {
                PointerShieldMessage::PointerPress { .. } => Some(EditorMessage::DismissTransient),
                _ => None,
            })
            .key("wave-transient-dismiss-layer")
            .fill(),
        overlay,
    ])
    .key("wave-surface-stack")
    .fill()
}

fn wave_help_overlay() -> ViewNode<EditorMessage> {
    column([
        spacer()
            .height(WINDOW_HEADER_HEIGHT + WINDOW_DROPDOWN_GAP)
            .fill_width(),
        row([
            spacer().fill_width(),
            custom_widget_direct(WaveHelpWidget::new())
                .key("wave-help-panel")
                .width(WINDOW_HELP_WIDTH)
                .height(WINDOW_HELP_HEIGHT),
            spacer().width(WINDOW_HELP_RIGHT_INSET),
        ])
        .fill_width()
        .height(WINDOW_HELP_HEIGHT),
        spacer().fill_height(),
    ])
    .key("wave-help-overlay")
    .fill()
}

fn reduce_surface(state: &mut EditorState, message: EditorMessage) {
    match message {
        EditorMessage::ToggleWindowDropdown => {
            state.window_dropdown_open = !state.window_dropdown_open;
            state.help_open = false;
        }
        EditorMessage::SelectWindow(window) => {
            state.selected_window = window;
            state.window_dropdown_open = false;
            state.help_open = false;
            state.publication.set_selected_window(window);
        }
        EditorMessage::ToggleHelp => {
            state.help_open = !state.help_open;
            state.window_dropdown_open = false;
        }
        EditorMessage::DismissTransient => {
            state.window_dropdown_open = false;
            state.help_open = false;
        }
        EditorMessage::SetWaveformModifiers {
            command_held,
            shift_held,
        } => {
            state.waveform_command_held = command_held;
            state.waveform_shift_held = shift_held;
            if state.active_waveform_offset.is_some() {
                state.active_waveform_offset = None;
            }
        }
        EditorMessage::BeginWaveformOffset { pointer_x } => {
            state.waveform_command_held = true;
            state.waveform_shift_held = true;
            state.active_waveform_offset = Some(WaveformOffsetDrag {
                origin_offset: state.waveform_offset,
                start_pointer_x: pointer_x,
            });
        }
        EditorMessage::DragWaveformOffset { delta } => {
            if let Some(drag) = state.active_waveform_offset {
                set_waveform_offset(state, drag.origin_offset + delta);
            }
        }
        EditorMessage::EndWaveformOffset { delta } => {
            if let Some(drag) = state.active_waveform_offset.take() {
                set_waveform_offset(state, drag.origin_offset + delta);
            }
        }
        EditorMessage::CancelWaveformOffset => {
            if let Some(drag) = state.active_waveform_offset.take() {
                set_waveform_offset(state, drag.origin_offset);
            }
        }
        EditorMessage::ResetWaveformOffset => {
            state.active_waveform_offset = None;
            set_waveform_offset(state, 0.0);
        }
    }
}

fn set_waveform_offset(state: &mut EditorState, offset: f32) {
    state.waveform_offset = normalize_waveform_offset(offset);
}

/// Retained editor implementation shared by CLAP and VST3 hosts.
pub struct WaveEditor {
    publication: Arc<WaveformPublication>,
    runtime: EditorRuntime,
    theme: ThemeTokens,
    paint_plan: SurfacePaintPlan,
    last_redraw_revision: u64,
}

impl WaveEditor {
    /// Construct a retained editor backed by the audio-thread publication.
    pub fn new(publication: Arc<WaveformPublication>) -> Self {
        let theme = ThemeTokens::dark();
        Self {
            publication: Arc::clone(&publication),
            runtime: EditorRuntime::new_declarative(
                EditorState {
                    publication,
                    view: WaveformView::default(),
                    selected_window: WindowLength::DEFAULT,
                    window_dropdown_open: false,
                    help_open: false,
                    waveform_offset: 0.0,
                    waveform_command_held: false,
                    waveform_shift_held: false,
                    active_waveform_offset: None,
                },
                Vector2::new(WINDOW_WIDTH as f32, WINDOW_HEIGHT as f32),
                project_surface,
                reduce_surface,
            ),
            theme,
            paint_plan: SurfacePaintPlan::empty(&theme),
            last_redraw_revision: 0,
        }
    }

    fn transient_state(&self) -> (bool, bool) {
        let state = self.runtime.bridge().state();
        (state.window_dropdown_open, state.help_open)
    }

    fn restore_focus_after_transient_close(
        &mut self,
        was_window_dropdown_open: bool,
        was_help_open: bool,
    ) {
        let target = {
            let state = self.runtime.bridge().state();
            if was_window_dropdown_open && !state.window_dropdown_open && !state.help_open {
                Some(WINDOW_DROPDOWN_WIDGET_ID)
            } else if was_help_open && !state.help_open && !state.window_dropdown_open {
                Some(WINDOW_HELP_BUTTON_WIDGET_ID)
            } else {
                None
            }
        };
        if let Some(target) = target {
            let _ = self.runtime.focus_widget(target);
        }
    }
}

impl toybox::radiant_gui::RadiantEditor for WaveEditor {
    fn resize(&mut self, width: u32, height: u32) {
        let _ = self.runtime.dispatch_event(Event::resize(Vector2::new(
            width.max(1) as f32,
            height.max(1) as f32,
        )));
    }

    fn dispatch_event(&mut self, event: Event) {
        let (was_window_dropdown_open, was_help_open) = self.transient_state();
        let _ = self.runtime.dispatch_event(event);
        self.restore_focus_after_transient_close(was_window_dropdown_open, was_help_open);
    }

    fn paint_plan(&mut self) -> &SurfacePaintPlan {
        let revision = self.publication.redraw_revision();
        if revision != self.last_redraw_revision {
            self.last_redraw_revision = revision;
            self.runtime.refresh();
        }
        let _ = self
            .runtime
            .borrowed_frame_into(&self.theme, &mut self.paint_plan);
        &self.paint_plan
    }

    fn needs_realtime_redraw(&self) -> bool {
        self.publication.redraw_revision() != self.last_redraw_revision
    }

    fn dispatch_key_press(&mut self, key: WidgetKey) -> bool {
        let (was_window_dropdown_open, was_help_open) = self.transient_state();
        let handled = self.runtime.dispatch_event(Event::key_press(key)).is_some();
        self.restore_focus_after_transient_close(was_window_dropdown_open, was_help_open);
        handled
    }

    fn dispatch_character(&mut self, character: char) -> bool {
        let (was_window_dropdown_open, was_help_open) = self.transient_state();
        let handled = self
            .runtime
            .dispatch_event(Event::character(character))
            .is_some();
        self.restore_focus_after_transient_close(was_window_dropdown_open, was_help_open);
        handled
    }

    fn cancel_text_entry(&mut self) -> bool {
        let (was_window_dropdown_open, was_help_open) = self.transient_state();
        if !was_window_dropdown_open && !was_help_open {
            return false;
        }
        let _ = self
            .runtime
            .dispatch_message(EditorMessage::DismissTransient);
        self.restore_focus_after_transient_close(was_window_dropdown_open, was_help_open);
        true
    }
}

/// Construct the CLAP/VST3 host facade for the retained editor.
pub fn new_gui(publication: Arc<WaveformPublication>) -> toybox::radiant_gui::RadiantHostedGui {
    toybox::radiant_gui::RadiantHostedGui::new(
        "WaveRadiantEditor",
        WaveEditor::new(publication),
        WINDOW_WIDTH,
        WINDOW_HEIGHT,
    )
    .with_size_contract((640, 360), (WINDOW_WIDTH, WINDOW_HEIGHT), (1600, 1000))
}

/// Return the preferred logical editor size used by the CLAP GUI callbacks.
pub const fn preferred_window_size() -> (u32, u32) {
    (WINDOW_WIDTH, WINDOW_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{EnvelopePoint, SnapshotMode};
    use radiant::widgets::PointerModifiers;
    use toybox::radiant_gui::RadiantEditor;

    fn test_editor() -> WaveEditor {
        WaveEditor::new(Arc::new(WaveformPublication::new()))
    }

    fn widget_rect(editor: &WaveEditor, widget_id: u64) -> Rect {
        editor
            .runtime
            .layout()
            .rects
            .get(&widget_id)
            .copied()
            .unwrap_or_else(|| panic!("widget {widget_id:#x} should be laid out"))
    }

    fn center(rect: Rect) -> Point {
        Point::new(
            (rect.min.x + rect.max.x) * 0.5,
            (rect.min.y + rect.max.y) * 0.5,
        )
    }

    fn click(editor: &mut WaveEditor, point: Point) {
        editor.dispatch_event(Event::primary_press(point));
        editor.dispatch_event(Event::primary_release(point));
    }

    fn assert_inside(rect: Rect, width: u32, height: u32) {
        assert!(
            rect.min.x >= 0.0,
            "rectangle starts outside left edge: {rect:?}"
        );
        assert!(
            rect.min.y >= 0.0,
            "rectangle starts outside top edge: {rect:?}"
        );
        assert!(
            rect.max.x <= width as f32,
            "rectangle clips right edge: {rect:?} in {width}x{height}"
        );
        assert!(
            rect.max.y <= height as f32,
            "rectangle clips bottom edge: {rect:?} in {width}x{height}"
        );
    }

    fn painted_texts(widget: &WaveformWidget) -> Vec<String> {
        let mut primitives = Vec::new();
        widget.append_paint(
            &mut primitives,
            Rect::from_xy_size(0.0, 0.0, 640.0, 360.0),
            &LayoutOutput::default(),
            &ThemeTokens::dark(),
        );
        primitives
            .into_iter()
            .filter_map(|primitive| match primitive {
                PaintPrimitive::Text(text) => Some(text.text.as_str().to_owned()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn waveform_chart_reserves_original_footer_at_supported_sizes() {
        let widget = WaveformWidget::new(WaveformView::default());

        for (width, height) in [(640, 360), (WINDOW_WIDTH, WINDOW_HEIGHT), (1600, 1000)] {
            let bounds = Rect::from_xy_size(0.0, 0.0, width as f32, height as f32);
            let plan = widget.paint_plan_with_defaults(bounds);
            let chart = plan
                .stroke_rects_for_widget(widget.common.id)
                .next()
                .map(|stroke| stroke.rect)
                .unwrap_or_else(|| {
                    panic!("waveform chart stroke should exist at {width}x{height}")
                });

            let logical_height = height as f32;
            let header_height = 34.0_f32.min(logical_height * 0.2);
            let footer_height = 28.0_f32.min(logical_height * 0.12);
            let expected_height = (logical_height - header_height - footer_height - 16.0).max(1.0);

            assert!((chart.min.x - 16.0).abs() < 0.001);
            assert!((chart.min.y - header_height).abs() < 0.001);
            assert!((chart.width() - (width as f32 - 32.0).max(1.0)).abs() < 0.001);
            assert!((chart.height() - expected_height).abs() < 0.001);
        }
    }

    fn live_view(window: WindowLength, sample_count: usize, live_revision: u64) -> WaveformView {
        WaveformView {
            live_revision,
            live_valid: true,
            live_window: window,
            live_sample_count: sample_count,
            target_sample_count: 1024,
            is_playing: true,
            ..WaveformView::default()
        }
    }

    #[test]
    fn status_text_is_exact_for_live_held_completed_and_waiting_presentations() {
        let live = WaveformWidget::new(live_view(WindowLength::FourBeats, 224, 1));
        assert_eq!(live.status_text(), "LIVE · 4 beats");

        let held_retained = WaveformWidget::new(WaveformView {
            display_revision: 1,
            display_valid: true,
            display_window: WindowLength::TwoBeats,
            display_sample_count: 1,
            display_target_sample_count: 1024,
            is_playing: true,
            ..WaveformView::default()
        });
        assert_eq!(held_retained.status_text(), "HELD · 2 beats");

        let held_completed = WaveformWidget::new(WaveformView {
            snapshot_revision: 1,
            snapshot_window: WindowLength::EightBeats,
            sample_count: 1024,
            is_playing: true,
            ..WaveformView::default()
        });
        assert_eq!(held_completed.status_text(), "HELD · 8 beats");

        let waiting = WaveformWidget::new(WaveformView::default());
        assert_eq!(waiting.status_text(), "WAITING");

        let waiting_for_new_capture = WaveformWidget::new(WaveformView {
            display_revision: 1,
            display_valid: true,
            display_window: WindowLength::TwoBeats,
            display_sample_count: 1,
            display_target_sample_count: 1024,
            is_playing: true,
            ..WaveformView::default()
        });
        assert_eq!(waiting_for_new_capture.status_text(), "HELD · 2 beats");

        for (widget, expected) in [
            (&live, "LIVE · 4 beats"),
            (&held_retained, "HELD · 2 beats"),
            (&held_completed, "HELD · 8 beats"),
            (&waiting, "WAITING"),
            (&waiting_for_new_capture, "HELD · 2 beats"),
        ] {
            assert_eq!(painted_texts(widget), vec![expected.to_owned()]);
        }
        for removed in [
            "RETAINED PREFIX",
            "% captured",
            "samples",
            "complete",
            "BPM",
            "NO TEMPO",
            "SYNCED",
            "UNSYNCED",
            "WAITING FOR WINDOW",
        ] {
            assert!(
                painted_texts(&live)
                    .into_iter()
                    .all(|text| !text.contains(removed)),
                "removed text still visible: {removed}"
            );
        }
    }

    #[test]
    fn status_readout_semantics_are_polite_and_stable_during_progressive_live_capture() {
        let first = WaveformWidget::new(live_view(WindowLength::FourBeats, 224, 1));
        let second = WaveformWidget::new(live_view(WindowLength::FourBeats, 768, 2));

        let semantics = first.automation_semantics();
        assert_eq!(semantics.role, AutomationRole::Readout);
        assert_eq!(semantics.label.as_deref(), Some("Capture status"));
        assert_eq!(semantics.value_text.as_deref(), Some("LIVE · 4 beats"));
        assert_eq!(semantics.live_region, AutomationLiveRegion::Polite);
        assert!(!semantics.focusable);

        let expected_revision = Some(WidgetSemanticsRevision::exact(StatusPresentation::Live(
            WindowLength::FourBeats,
        )));
        assert_eq!(first.capabilities().semantics_revision(), expected_revision);
        assert_eq!(
            second.capabilities().semantics_revision(),
            expected_revision
        );

        let held_stopped = WaveformWidget::new(WaveformView {
            snapshot_revision: 1,
            snapshot_window: WindowLength::FourBeats,
            sample_count: 1,
            is_playing: false,
            ..WaveformView::default()
        });
        let held_playing = WaveformWidget::new(WaveformView {
            snapshot_revision: 1,
            snapshot_window: WindowLength::FourBeats,
            sample_count: 1,
            is_playing: true,
            ..WaveformView::default()
        });
        assert_eq!(held_stopped.status_text(), "HELD · 4 beats");
        assert_eq!(held_playing.status_text(), "HELD · 4 beats");
        assert_eq!(
            held_stopped.automation_value_text().as_deref(),
            Some("HELD · 4 beats")
        );
        assert_eq!(
            held_playing.automation_value_text().as_deref(),
            Some("HELD · 4 beats")
        );
        assert_eq!(
            held_stopped.capabilities().semantics_revision(),
            held_playing.capabilities().semantics_revision()
        );

        let empty_snapshot = WaveformWidget::new(WaveformView {
            snapshot_window: WindowLength::OneBeat,
            display_window: WindowLength::TwoBeats,
            ..WaveformView::default()
        });
        let empty_display = WaveformWidget::new(WaveformView {
            snapshot_window: WindowLength::EightBeats,
            display_window: WindowLength::FourBeats,
            is_playing: true,
            ..WaveformView::default()
        });
        assert_eq!(empty_snapshot.status_text(), "WAITING");
        assert_eq!(empty_display.status_text(), "WAITING");
        assert_eq!(
            empty_snapshot.automation_value_text().as_deref(),
            Some("WAITING")
        );
        assert_eq!(
            empty_display.automation_value_text().as_deref(),
            Some("WAITING")
        );
        assert_eq!(
            empty_snapshot.capabilities().semantics_revision(),
            Some(WidgetSemanticsRevision::exact(StatusPresentation::Waiting))
        );
        assert_eq!(
            empty_snapshot.capabilities().semantics_revision(),
            empty_display.capabilities().semantics_revision()
        );
    }

    #[test]
    fn waveform_offset_format_uses_signed_samples_and_rounded_milliseconds() {
        assert_eq!(
            format_waveform_offset(0.0, 48_000, 48_000.0),
            "0 samples · 0.00 ms"
        );
        assert_eq!(
            format_waveform_offset(0.25, 48_000, 48_000.0),
            "+12000 samples · +250.00 ms"
        );
        assert_eq!(
            format_waveform_offset(0.75, 48_000, 48_000.0),
            "-12000 samples · -250.00 ms"
        );
        assert_eq!(
            format_waveform_offset(0.5, 48_000, 48_000.0),
            "+24000 samples · +500.00 ms"
        );
        assert_eq!(
            format_waveform_offset(0.12345, 1_000, 44_100.0),
            "+123 samples · +2.79 ms"
        );
        assert_eq!(
            format_waveform_offset(0.999999, 1, 44_100.0),
            "0 samples · 0.00 ms"
        );
        assert_eq!(
            format_waveform_offset(0.999, 1_000, 1_000_000_000.0),
            "-1 samples · 0.00 ms"
        );
    }

    #[test]
    fn offset_sample_count_uses_the_active_waveform_window() {
        assert_eq!(
            offset_sample_count(live_view(WindowLength::FourBeats, 224, 1)),
            1024
        );
        assert_eq!(
            offset_sample_count(WaveformView {
                display_valid: true,
                display_sample_count: 240,
                display_target_sample_count: 480,
                ..WaveformView::default()
            }),
            480
        );
        assert_eq!(
            offset_sample_count(WaveformView {
                sample_count: 960,
                ..WaveformView::default()
            }),
            960
        );
    }

    #[test]
    fn zero_marker_is_stationary_at_the_chart_origin_for_each_offset() {
        let bounds = Rect::from_xy_size(0.0, 0.0, 640.0, 360.0);
        let view = WaveformView {
            snapshot_revision: 1,
            sample_count: ENVELOPE_BINS,
            envelope: [EnvelopePoint {
                min: -0.3,
                max: 0.5,
            }; ENVELOPE_BINS],
            ..WaveformView::default()
        };
        let chart = WaveformWidget::chart_rect(bounds);
        let expected = zero_marker_rect(chart);
        for offset in [0.0, 0.25, 0.75] {
            let widget = WaveformWidget::new_with_interaction(view, offset, false, false, None);
            let mut primitives = Vec::new();
            widget.append_paint(
                &mut primitives,
                bounds,
                &LayoutOutput::default(),
                &ThemeTokens::dark(),
            );
            let marker = primitives
                .iter()
                .find_map(|primitive| match primitive {
                    PaintPrimitive::FillRect(fill) if fill.rect == expected => Some(fill),
                    _ => None,
                })
                .expect("true-zero marker should be painted");
            assert_eq!((marker.rect.min.x + marker.rect.max.x) * 0.5, chart.min.x);
            assert_eq!(marker.rect.max.y, chart.max.y);
        }
    }

    #[test]
    fn waveform_offset_readout_is_accessible_without_live_announcements_or_pointer_capture() {
        let readout = WaveformOffsetReadoutWidget::new(0.25, 48_000, 48_000.0);
        let semantics = readout.automation_semantics();
        assert_eq!(semantics.role, AutomationRole::Readout);
        assert_eq!(semantics.label.as_deref(), Some("Waveform offset"));
        assert_eq!(
            semantics.value_text.as_deref(),
            Some("+12000 samples · +250.00 ms")
        );
        assert_eq!(semantics.live_region, AutomationLiveRegion::None);
        assert!(!semantics.focusable);

        let bounds = Rect::from_xy_size(0.0, 0.0, 640.0, 360.0);
        assert!(
            !readout.accepts_pointer_input(&WidgetInput::primary_press(Point::new(320.0, 200.0)))
        );
        let mut primitives = Vec::new();
        readout.append_paint(
            &mut primitives,
            bounds,
            &LayoutOutput::default(),
            &ThemeTokens::dark(),
        );
        assert!(primitives.iter().any(|primitive| matches!(
            primitive,
            PaintPrimitive::Text(text) if text.text.as_str() == "+12000 samples · +250.00 ms"
        )));
    }

    #[test]
    fn primary_shift_double_click_reset_is_exact_and_preserves_other_gestures() {
        let bounds = Rect::from_xy_size(0.0, 0.0, 640.0, 360.0);
        let chart = WaveformWidget::chart_rect(bounds);
        let position = Point::new(
            chart.min.x + chart.width() * 0.35,
            chart.min.y + chart.height() * 0.5,
        );
        let exact = PointerModifiers {
            command: true,
            shift: true,
            ..PointerModifiers::default()
        };
        let with_alt = PointerModifiers { alt: true, ..exact };
        let mut widget = WaveformWidget::new_with_interaction(
            WaveformView::default(),
            0.375,
            false,
            false,
            None,
        );

        assert!(
            widget
                .handle_input(
                    bounds,
                    WidgetInput::pointer_double_click(position, PointerButton::Primary, exact),
                )
                .and_then(|output| output.typed_copied::<EditorMessage>())
                == Some(EditorMessage::ResetWaveformOffset)
        );
        assert!(
            widget
                .handle_input(
                    bounds,
                    WidgetInput::pointer_double_click(position, PointerButton::Primary, with_alt),
                )
                .is_none()
        );
        assert!(
            widget
                .handle_input(
                    bounds,
                    WidgetInput::pointer_double_click(position, PointerButton::Secondary, exact),
                )
                .is_none()
        );
        assert!(
            widget
                .handle_input(bounds, WidgetInput::primary_double_click(position))
                .is_none()
        );
        assert!(
            widget
                .handle_input(
                    bounds,
                    WidgetInput::pointer_double_click(
                        Point::new(bounds.min.x + 2.0, bounds.min.y + 2.0),
                        PointerButton::Primary,
                        exact,
                    ),
                )
                .is_none()
        );
    }

    #[test]
    fn reset_reducer_uses_the_same_normalized_offset_path_as_dragging() {
        let publication = Arc::new(WaveformPublication::new());
        let mut state = EditorState {
            publication,
            view: WaveformView::default(),
            selected_window: WindowLength::DEFAULT,
            window_dropdown_open: false,
            help_open: false,
            waveform_offset: 0.25,
            waveform_command_held: false,
            waveform_shift_held: false,
            active_waveform_offset: None,
        };
        reduce_surface(
            &mut state,
            EditorMessage::BeginWaveformOffset { pointer_x: 0.2 },
        );
        reduce_surface(&mut state, EditorMessage::DragWaveformOffset { delta: 0.3 });
        assert!((state.waveform_offset - 0.55).abs() < 1.0e-6);
        reduce_surface(&mut state, EditorMessage::ResetWaveformOffset);
        assert_eq!(state.waveform_offset, 0.0);
        assert!(state.active_waveform_offset.is_none());
    }

    #[test]
    fn editor_routes_exact_reset_through_the_waveform_beneath_readout_overlay() {
        let mut editor = test_editor();
        editor.runtime.bridge_mut().state_mut().waveform_offset = 0.375;
        editor.runtime.refresh();
        let waveform = widget_rect(&editor, WAVEFORM_WIDGET_ID);
        let chart = WaveformWidget::chart_rect(waveform);
        let position = Point::new(
            chart.min.x + chart.width() * 0.4,
            chart.min.y + chart.height() * 0.5,
        );
        let modifiers = PointerModifiers {
            command: true,
            shift: true,
            ..PointerModifiers::default()
        };
        editor.dispatch_event(Event::PointerDoubleClick {
            position,
            button: PointerButton::Primary,
            modifiers,
        });
        assert_eq!(editor.runtime.bridge().state().waveform_offset, 0.0);
        let plan = editor.paint_plan().clone();
        assert!(plan.contains_text("0 samples · 0.00 ms"));
    }

    #[test]
    fn offset_readout_republishes_with_the_active_sample_rate() {
        let publication = Arc::new(WaveformPublication::new());
        let envelope = [EnvelopePoint {
            min: -0.2,
            max: 0.2,
        }; ENVELOPE_BINS];
        publication.publish_envelope(
            SnapshotMode::Synced,
            WindowLength::FourBeats,
            Some(120.0),
            Some(0),
            48_000,
            &envelope,
        );
        let mut editor = WaveEditor::new(Arc::clone(&publication));
        editor.runtime.bridge_mut().state_mut().waveform_offset = 0.25;
        editor.runtime.refresh();
        assert!(
            editor
                .paint_plan()
                .contains_text("+12000 samples · +250.00 ms")
        );

        assert!(publication.set_sample_rate(44_100.0));
        let plan = editor.paint_plan().clone();
        assert!(plan.contains_text("+12000 samples · +272.11 ms"));
    }

    #[test]
    fn invalid_live_preview_keeps_drawing_the_latest_completed_envelope() {
        let view = WaveformView {
            snapshot_revision: 1,
            snapshot_mode: SnapshotMode::Synced,
            snapshot_window: WindowLength::FourBeats,
            sample_count: 12_345,
            envelope: std::array::from_fn(|_| EnvelopePoint {
                min: -0.35,
                max: 0.35,
            }),
            is_playing: true,
            ..WaveformView::default()
        };

        let widget = WaveformWidget::new(view);
        assert!(!widget.showing_live_preview());
        assert_eq!(widget.displayed_window(), WindowLength::FourBeats);
        assert_eq!(widget.status_text(), "HELD · 4 beats");

        let mut primitives = Vec::new();
        widget.append_paint(
            &mut primitives,
            Rect::from_xy_size(0.0, 0.0, 640.0, 360.0),
            &LayoutOutput::default(),
            &ThemeTokens::dark(),
        );
        assert!(primitives.iter().any(|primitive| {
            matches!(
                primitive,
                PaintPrimitive::StrokePolyline(PaintStrokePolyline { points, .. })
                    if !points.is_empty()
            )
        }));
    }

    #[test]
    fn invalidated_retained_prefix_draws_without_a_completed_frame() {
        let point = EnvelopePoint {
            min: -0.45,
            max: 0.8,
        };
        let view = WaveformView {
            display_revision: 1,
            display_valid: true,
            display_mode: SnapshotMode::Synced,
            display_window: WindowLength::TwoBeats,
            display_tempo_bpm: Some(120.0),
            display_beat_index: Some(4),
            display_sample_count: 1,
            display_target_sample_count: 1024,
            display_envelope: [point; ENVELOPE_BINS],
            is_playing: true,
            ..WaveformView::default()
        };

        let widget = WaveformWidget::new(view);
        assert!(!widget.showing_live_preview());
        assert!(widget.showing_retained_preview());
        assert_eq!(widget.displayed_window(), WindowLength::TwoBeats);
        assert_eq!(widget.status_text(), "HELD · 2 beats");

        let mut primitives = Vec::new();
        widget.append_paint(
            &mut primitives,
            Rect::from_xy_size(0.0, 0.0, 640.0, 360.0),
            &LayoutOutput::default(),
            &ThemeTokens::dark(),
        );
        let waveform_lines = primitives
            .iter()
            .filter_map(|primitive| match primitive {
                PaintPrimitive::StrokePolyline(PaintStrokePolyline { points, .. }) => {
                    Some(points.len())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(waveform_lines, vec![1, 1]);
    }

    #[test]
    fn invalidated_retained_prefix_overlays_completed_tail_and_keeps_display_selection() {
        let prefix = EnvelopePoint {
            min: -0.8,
            max: 0.7,
        };
        let completed = EnvelopePoint {
            min: -0.2,
            max: 0.25,
        };
        let view = WaveformView {
            snapshot_revision: 7,
            snapshot_mode: SnapshotMode::Synced,
            snapshot_window: WindowLength::FourBeats,
            snapshot_tempo_bpm: Some(120.0),
            snapshot_beat_index: Some(0),
            sample_count: ENVELOPE_BINS,
            envelope: [completed; ENVELOPE_BINS],
            display_revision: 8,
            display_valid: true,
            display_mode: SnapshotMode::Synced,
            display_window: WindowLength::TwoBeats,
            display_tempo_bpm: Some(118.0),
            display_beat_index: Some(4),
            display_sample_count: ENVELOPE_BINS / 2,
            display_target_sample_count: ENVELOPE_BINS,
            display_envelope: [prefix; ENVELOPE_BINS],
            is_playing: true,
            ..WaveformView::default()
        };

        let widget = WaveformWidget::new(view);
        assert!(!widget.showing_live_preview());
        assert!(widget.showing_retained_preview());
        assert_eq!(widget.displayed_window(), WindowLength::TwoBeats);
        assert_eq!(widget.status_text(), "HELD · 2 beats");

        let mut primitives = Vec::new();
        widget.append_paint(
            &mut primitives,
            Rect::from_xy_size(0.0, 0.0, 640.0, 360.0),
            &LayoutOutput::default(),
            &ThemeTokens::dark(),
        );
        let waveform_lines = primitives
            .iter()
            .filter_map(|primitive| match primitive {
                PaintPrimitive::StrokePolyline(PaintStrokePolyline { points, .. }) => Some(points),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(waveform_lines.len(), 2);
        assert_eq!(waveform_lines[0].len(), ENVELOPE_BINS);
        assert_eq!(waveform_lines[1].len(), ENVELOPE_BINS);
        assert!(
            waveform_lines[0][ENVELOPE_BINS / 2 - 1].y < waveform_lines[0][ENVELOPE_BINS / 2].y
        );
        assert!(
            waveform_lines[1][ENVELOPE_BINS / 2 - 1].y > waveform_lines[1][ENVELOPE_BINS / 2].y
        );
    }

    #[test]
    fn partial_live_preview_overwrites_completed_envelope_without_an_empty_tail() {
        let point = EnvelopePoint {
            min: -0.35,
            max: 0.35,
        };
        let view = WaveformView {
            snapshot_revision: 1,
            snapshot_mode: SnapshotMode::Synced,
            snapshot_window: WindowLength::FourBeats,
            sample_count: 1024,
            envelope: [point; ENVELOPE_BINS],
            live_revision: 1,
            live_valid: true,
            live_mode: SnapshotMode::Synced,
            live_window: WindowLength::FourBeats,
            live_sample_count: 1,
            target_sample_count: 1024,
            live_envelope: [EnvelopePoint::default(); ENVELOPE_BINS],
            is_playing: true,
            ..WaveformView::default()
        };
        let widget = WaveformWidget::new(view);
        assert_eq!(widget.status_text(), "LIVE · 4 beats");
        let mut primitives = Vec::new();
        widget.append_paint(
            &mut primitives,
            Rect::from_xy_size(0.0, 0.0, 640.0, 360.0),
            &LayoutOutput::default(),
            &ThemeTokens::dark(),
        );

        let waveform_lines = primitives
            .iter()
            .filter_map(|primitive| match primitive {
                PaintPrimitive::StrokePolyline(PaintStrokePolyline { points, .. }) => {
                    Some(points.len())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(waveform_lines, vec![ENVELOPE_BINS, ENVELOPE_BINS]);
    }

    #[test]
    fn header_brand_version_and_hit_targets_hold_at_supported_sizes() {
        let mut editor = test_editor();
        assert_eq!(WINDOW_VERSION_LABEL, env!("CARGO_PKG_VERSION"));

        for (width, height) in [(640, 360), (WINDOW_WIDTH, WINDOW_HEIGHT), (1600, 1000)] {
            editor.resize(width, height);
            let plan = editor.paint_plan().clone();
            for label in ["PortalSurfer", "/", "WAVE", WINDOW_VERSION_LABEL] {
                assert_eq!(
                    plan.text_labels()
                        .filter(|visible| *visible == label)
                        .count(),
                    1,
                    "expected one exact {label:?} label at {width}x{height}"
                );
            }
            assert_eq!(
                plan.first_text_color("WAVE"),
                Some(ThemeTokens::dark().accent_copper)
            );
            assert!(!plan.contains_text("WAVE  /  BEAT-SYNCED WAVEFORM"));

            let title_rect = plan.first_text_rect("WAVE").expect("WAVE brand text");
            let version_rect = plan
                .first_text_rect(WINDOW_VERSION_LABEL)
                .expect("package version text");
            assert!(version_rect.min.y >= title_rect.max.y - 0.1);
            assert!(version_rect.max.y <= WINDOW_HEADER_HEIGHT);

            let trigger = widget_rect(&editor, WINDOW_DROPDOWN_WIDGET_ID);
            assert!((trigger.min.x - WINDOW_DROPDOWN_X).abs() < 0.1);
            assert!((trigger.width() - WINDOW_DROPDOWN_WIDTH).abs() < 0.1);
            assert_inside(trigger, width, height);

            let help = widget_rect(&editor, WINDOW_HELP_BUTTON_WIDGET_ID);
            assert!(help.width() >= WINDOW_HELP_BUTTON_SIZE);
            assert!(help.height() >= WINDOW_HELP_BUTTON_SIZE);
            assert_inside(help, width, height);
        }
    }

    #[test]
    fn wave_help_is_accessible_painted_and_activates_with_pointer_or_keyboard() {
        let bounds = Rect::from_xy_size(0.0, 0.0, WINDOW_HELP_BUTTON_SIZE, WINDOW_HELP_BUTTON_SIZE);
        let mut button = WaveHelpButtonWidget::new();
        let semantics = button.automation_semantics();
        assert_eq!(semantics.role, AutomationRole::Button);
        assert_eq!(semantics.label.as_deref(), Some("Show WAVE help"));
        assert_eq!(
            semantics.description.as_deref(),
            Some("Open the WAVE help panel")
        );
        assert!(semantics.focusable);

        button.handle_input(bounds, WidgetInput::FocusChanged(true));
        for key in [WidgetKey::Enter, WidgetKey::Space] {
            assert_eq!(
                button
                    .handle_input(bounds, WidgetInput::KeyPress(key))
                    .and_then(|output| output.typed_copied::<ButtonMessage>()),
                Some(ButtonMessage::Activate)
            );
        }

        let mut pointer_button = WaveHelpButtonWidget::new();
        assert!(
            pointer_button
                .handle_input(bounds, WidgetInput::primary_press(Point::new(14.0, 14.0)))
                .is_none()
        );
        assert_eq!(
            pointer_button
                .handle_input(bounds, WidgetInput::primary_release(Point::new(14.0, 14.0)))
                .and_then(|output| output.typed_copied::<ButtonMessage>()),
            Some(ButtonMessage::Activate)
        );

        let theme = ThemeTokens::dark();
        let mut button_primitives = Vec::new();
        button.append_paint(
            &mut button_primitives,
            bounds,
            &LayoutOutput::default(),
            &theme,
        );
        assert!(button_primitives.iter().any(|primitive| matches!(
            primitive,
            PaintPrimitive::Text(text) if text.text.as_str() == "?"
        )));
        button.handle_input(bounds, WidgetInput::FocusChanged(true));
        let mut focused_primitives = Vec::new();
        button.append_paint(
            &mut focused_primitives,
            bounds,
            &LayoutOutput::default(),
            &theme,
        );
        assert!(focused_primitives.iter().any(|primitive| matches!(
            primitive,
            PaintPrimitive::StrokeRect(stroke) if stroke.color == theme.accent_warning
        )));

        let panel = WaveHelpWidget::new();
        let panel_semantics = panel.automation_semantics();
        assert_eq!(panel_semantics.role, AutomationRole::Text);
        assert_eq!(panel_semantics.label.as_deref(), Some("WAVE help"));
        assert_eq!(
            panel_semantics.description.as_deref(),
            Some("Supported WAVE keyboard and pointer interactions")
        );
        assert!(!panel_semantics.focusable);
        let mut panel_primitives = Vec::new();
        panel.append_paint(
            &mut panel_primitives,
            Rect::from_xy_size(0.0, 0.0, WINDOW_HELP_WIDTH, WINDOW_HELP_HEIGHT),
            &LayoutOutput::default(),
            &theme,
        );
        assert!(panel_primitives.iter().any(|primitive| matches!(
            primitive,
            PaintPrimitive::Text(text) if text.text.as_str() == "WAVE HELP"
        )));
        for &(key, description) in &WINDOW_HELP_ROWS {
            assert!(panel_primitives.iter().any(|primitive| matches!(
                primitive,
                PaintPrimitive::Text(text) if text.text.as_str() == key
            )));
            assert!(panel_primitives.iter().any(|primitive| matches!(
                primitive,
                PaintPrimitive::Text(text) if text.text.as_str() == description
            )));
        }
        assert!(!panel_primitives.iter().any(|primitive| matches!(
            primitive,
            PaintPrimitive::Text(text) if text.text.as_str().contains("Arrow")
        )));
        assert!(panel_primitives.iter().any(|primitive| matches!(
            primitive,
            PaintPrimitive::FillRect(fill)
                if (fill.rect.width() - (WINDOW_HELP_WIDTH - 2.0)).abs() < 0.1
        )));
    }

    #[test]
    fn window_dropdown_preserves_all_choices_selection_and_left_anchor() {
        let publication = Arc::new(WaveformPublication::new());
        let mut editor = WaveEditor::new(Arc::clone(&publication));
        let trigger = widget_rect(&editor, WINDOW_DROPDOWN_WIDGET_ID);
        assert!((trigger.min.x - WINDOW_DROPDOWN_X).abs() < 0.1);
        let expected_labels = ["1:4", "1:2", "1:1", "2:1"];
        let closed_plan = editor.paint_plan().clone();
        assert_eq!(
            closed_plan
                .text_runs()
                .filter(|run| expected_labels.contains(&run.text.as_str()))
                .map(|run| run.text.as_str())
                .collect::<Vec<_>>(),
            vec!["1:4"]
        );

        click(&mut editor, center(trigger));
        assert!(editor.runtime.bridge().state().window_dropdown_open);
        assert!(!editor.runtime.bridge().state().help_open);
        assert_eq!(
            editor.runtime.focused_widget(),
            Some(WINDOW_DROPDOWN_WIDGET_ID)
        );
        let plan = editor.paint_plan().clone();
        let mut menu_labels = plan
            .text_runs()
            .filter(|run| {
                run.rect.min.y > trigger.max.y && expected_labels.contains(&run.text.as_str())
            })
            .map(|run| (run.rect.min.y, run.text.as_str().to_owned()))
            .collect::<Vec<_>>();
        menu_labels.sort_by(|left, right| left.0.total_cmp(&right.0));
        assert_eq!(
            menu_labels
                .iter()
                .map(|(_, label)| label.as_str())
                .collect::<Vec<_>>(),
            expected_labels
        );
        for window in WindowLength::ALL {
            let option_rects = plan
                .text_runs()
                .filter(|run| run.text.as_str() == window.label() && run.rect.min.y > trigger.max.y)
                .map(|run| run.rect)
                .collect::<Vec<_>>();
            assert_eq!(option_rects.len(), 1, "missing menu option for {window:?}");
            assert_inside(option_rects[0], WINDOW_WIDTH, WINDOW_HEIGHT);
        }

        let selected = WindowLength::EightBeats;
        let option = plan
            .text_runs()
            .find(|run| run.text.as_str() == selected.label() && run.rect.min.y > trigger.max.y)
            .map(|run| run.rect)
            .expect("selected window option should be painted");
        click(&mut editor, center(option));
        assert_eq!(publication.selected_window(), selected);
        assert!(!editor.runtime.bridge().state().window_dropdown_open);
        assert!(!editor.runtime.bridge().state().help_open);
        assert_eq!(
            editor.runtime.focused_widget(),
            Some(WINDOW_DROPDOWN_WIDGET_ID)
        );
        let selected_plan = editor.paint_plan().clone();
        assert_eq!(
            selected_plan
                .text_runs()
                .filter(|run| expected_labels.contains(&run.text.as_str()))
                .map(|run| run.text.as_str())
                .collect::<Vec<_>>(),
            vec!["2:1"]
        );
    }

    #[test]
    fn transient_overlays_are_mutually_exclusive_and_restore_focus_after_dismissal() {
        let mut editor = test_editor();
        let initial_focus_order = editor.runtime.surface().keyboard_focus_order();
        assert!(initial_focus_order.contains(&WINDOW_DROPDOWN_WIDGET_ID));
        assert!(initial_focus_order.contains(&WINDOW_HELP_BUTTON_WIDGET_ID));

        let help = widget_rect(&editor, WINDOW_HELP_BUTTON_WIDGET_ID);
        click(&mut editor, center(help));
        assert!(editor.runtime.bridge().state().help_open);
        assert!(!editor.runtime.bridge().state().window_dropdown_open);
        assert_eq!(
            editor.runtime.focused_widget(),
            Some(WINDOW_HELP_BUTTON_WIDGET_ID)
        );
        assert_eq!(
            editor.runtime.surface().keyboard_focus_order(),
            initial_focus_order,
            "pointer shield must not enter keyboard focus order"
        );
        assert!(editor.paint_plan().contains_text("WAVE HELP"));

        assert!(editor.runtime.focus_widget(WINDOW_DROPDOWN_WIDGET_ID));
        assert!(editor.dispatch_key_press(WidgetKey::Enter));
        assert!(editor.runtime.bridge().state().window_dropdown_open);
        assert!(!editor.runtime.bridge().state().help_open);
        assert_eq!(
            editor.runtime.focused_widget(),
            Some(WINDOW_DROPDOWN_WIDGET_ID)
        );

        editor.dispatch_event(Event::primary_press(Point::new(
            12.0,
            WINDOW_HEADER_HEIGHT + 80.0,
        )));
        assert!(!editor.runtime.bridge().state().window_dropdown_open);
        assert!(!editor.runtime.bridge().state().help_open);
        assert_eq!(
            editor.runtime.focused_widget(),
            Some(WINDOW_DROPDOWN_WIDGET_ID)
        );
        assert!(!editor.cancel_text_entry());

        let trigger = widget_rect(&editor, WINDOW_DROPDOWN_WIDGET_ID);
        click(&mut editor, center(trigger));
        assert!(editor.runtime.bridge().state().window_dropdown_open);
        assert!(editor.cancel_text_entry());
        assert!(!editor.runtime.bridge().state().window_dropdown_open);
        assert_eq!(
            editor.runtime.focused_widget(),
            Some(WINDOW_DROPDOWN_WIDGET_ID)
        );

        assert!(editor.runtime.focus_widget(WINDOW_HELP_BUTTON_WIDGET_ID));
        assert!(editor.dispatch_key_press(WidgetKey::Space));
        assert!(editor.runtime.bridge().state().help_open);
        assert!(!editor.runtime.bridge().state().window_dropdown_open);
        assert!(editor.cancel_text_entry());
        assert!(!editor.runtime.bridge().state().help_open);
        assert_eq!(
            editor.runtime.focused_widget(),
            Some(WINDOW_HELP_BUTTON_WIDGET_ID)
        );
    }

    #[test]
    fn transient_overlays_fit_every_supported_viewport_after_real_interactions() {
        let menu_height = radiant::application::dropdown_menu_height(WindowLength::ALL.len());
        let menu_top =
            (WINDOW_DROPDOWN_TRIGGER_Y + WINDOW_DROPDOWN_TRIGGER_HEIGHT + WINDOW_DROPDOWN_GAP)
                .floor();

        for (width, height) in [(640, 360), (WINDOW_WIDTH, WINDOW_HEIGHT), (1600, 1000)] {
            let mut editor = test_editor();
            editor.resize(width, height);

            let trigger = widget_rect(&editor, WINDOW_DROPDOWN_WIDGET_ID);
            click(&mut editor, center(trigger));
            assert!(editor.runtime.bridge().state().window_dropdown_open);
            assert!(!editor.runtime.bridge().state().help_open);

            let dropdown_plan = editor.paint_plan().clone();
            for window in WindowLength::ALL {
                let option = dropdown_plan
                    .text_runs()
                    .find(|run| {
                        run.text.as_str() == window.label() && run.rect.min.y > trigger.max.y
                    })
                    .map(|run| run.rect)
                    .unwrap_or_else(|| panic!("missing {window:?} option at {width}x{height}"));
                assert_inside(option, width, height);
            }

            let menu_layout = editor
                .runtime
                .layout()
                .rects
                .values()
                .find(|rect| {
                    (rect.min.x - WINDOW_DROPDOWN_X).abs() < 0.1
                        && (rect.min.y - menu_top).abs() < 0.1
                        && (rect.width() - WINDOW_DROPDOWN_MENU_WIDTH).abs() < 0.1
                        && (rect.height() - menu_height).abs() < 0.1
                })
                .copied()
                .unwrap_or_else(|| {
                    panic!("dropdown layout should be anchored at {width}x{height}")
                });
            assert_inside(menu_layout, width, height);
            for (index, rect) in editor.runtime.layout().rects.values().enumerate() {
                assert_inside(*rect, width, height);
                assert!(
                    rect.max.x <= width as f32 && rect.max.y <= height as f32,
                    "layout rectangle {index} clips at {width}x{height}: {rect:?}"
                );
            }
            for (index, rect) in dropdown_plan.paint_rects().enumerate() {
                assert_inside(rect, width, height);
                assert!(
                    rect.max.x <= width as f32 && rect.max.y <= height as f32,
                    "dropdown paint rectangle {index} clips at {width}x{height}: {rect:?}"
                );
            }
            assert!(dropdown_plan.paint_rects().any(|rect| {
                (rect.min.x - WINDOW_DROPDOWN_X).abs() < 0.1
                    && (rect.min.y - menu_top).abs() < 0.1
                    && (rect.width() - WINDOW_DROPDOWN_MENU_WIDTH).abs() < 0.1
                    && (rect.height() - menu_height).abs() < 0.1
            }));

            assert!(editor.cancel_text_entry());
            let help = widget_rect(&editor, WINDOW_HELP_BUTTON_WIDGET_ID);
            click(&mut editor, center(help));
            assert!(editor.runtime.bridge().state().help_open);
            assert!(!editor.runtime.bridge().state().window_dropdown_open);

            let help_plan = editor.paint_plan().clone();
            assert!(help_plan.contains_text("WAVE HELP"));
            let panel_layout = editor
                .runtime
                .layout()
                .rects
                .values()
                .find(|rect| {
                    (rect.width() - WINDOW_HELP_WIDTH).abs() < 0.1
                        && (rect.height() - WINDOW_HELP_HEIGHT).abs() < 0.1
                })
                .copied()
                .unwrap_or_else(|| panic!("help layout should be present at {width}x{height}"));
            assert_inside(panel_layout, width, height);
            for (index, rect) in editor.runtime.layout().rects.values().enumerate() {
                assert_inside(*rect, width, height);
                assert!(
                    rect.max.x <= width as f32 && rect.max.y <= height as f32,
                    "help layout rectangle {index} clips at {width}x{height}: {rect:?}"
                );
            }
            for (index, rect) in help_plan.paint_rects().enumerate() {
                assert_inside(rect, width, height);
                assert!(
                    rect.max.x <= width as f32 && rect.max.y <= height as f32,
                    "help paint rectangle {index} clips at {width}x{height}: {rect:?}"
                );
            }
            assert!(help_plan.paint_rects().any(|rect| {
                (rect.width() - WINDOW_HELP_WIDTH).abs() < 0.1
                    && (rect.height() - WINDOW_HELP_HEIGHT).abs() < 0.1
            }));
            assert!(editor.cancel_text_entry());
        }
    }
}

#[cfg(all(test, feature = "screenshot-test"))]
mod screenshot_tests {
    use super::*;
    use crate::capture::SnapshotMode;
    use image::{ColorType, ImageFormat};
    use radiant::theme::DpiScale;
    use std::path::PathBuf;
    use toybox::radiant_gui::RadiantEditor;

    #[test]
    fn screenshot_renders_initial_ui() {
        let publication = Arc::new(WaveformPublication::new());
        let mut editor = WaveEditor::new(publication);
        let plan = editor.paint_plan().clone();
        assert_eq!(
            plan.text_labels()
                .filter(|label| *label == "WAITING")
                .count(),
            1
        );
        assert!(plan.contains_text(WindowLength::OneBeat.label()));
        assert_eq!(
            plan.text_labels()
                .filter(|label| *label == "PortalSurfer")
                .count(),
            1
        );
        assert_eq!(plan.text_labels().filter(|label| *label == "/").count(), 1);
        assert_eq!(
            plan.text_labels().filter(|label| *label == "WAVE").count(),
            1
        );
        assert_eq!(
            plan.text_labels()
                .filter(|label| *label == WINDOW_VERSION_LABEL)
                .count(),
            1
        );
        assert!(!plan.contains_text("WAVE  /  BEAT-SYNCED WAVEFORM"));
        assert!(plan.contains_text("0 samples · 0.00 ms"));
        for removed in [
            "RETAINED PREFIX",
            "% captured",
            "complete",
            "BPM",
            "NO TEMPO",
            "SYNCED",
            "UNSYNCED",
            "WAITING FOR WINDOW",
        ] {
            assert!(
                !plan.contains_text(removed),
                "removed text still visible: {removed}"
            );
        }
        let mut capture = toybox::radiant_gui::bundled_offscreen_capture(
            Vector2::new(WINDOW_WIDTH as f32, WINDOW_HEIGHT as f32),
            DpiScale::ONE,
        )
        .expect("Radiant offscreen capture should be available");
        let pixels = capture.capture(&plan).expect("screenshot should render");
        let root = std::env::var_os("TOYBOX_UI_SCREENSHOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/ui-screenshots"))
            .join("wave");
        std::fs::create_dir_all(&root).expect("screenshot directory should be writable");
        image::save_buffer_with_format(
            root.join("initial-ui-default.png"),
            &pixels,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            ColorType::Rgba8,
            ImageFormat::Png,
        )
        .expect("screenshot should be written");
    }

    #[test]
    fn screenshot_renders_zero_and_signed_waveform_offsets() {
        let publication = Arc::new(WaveformPublication::new());
        let envelope = [crate::capture::EnvelopePoint {
            min: -0.4,
            max: 0.6,
        }; ENVELOPE_BINS];
        publication.publish_envelope(
            SnapshotMode::Synced,
            WindowLength::FourBeats,
            Some(120.0),
            Some(0),
            48_000,
            &envelope,
        );
        publication.set_sample_rate(48_000.0);
        let mut editor = WaveEditor::new(publication);
        let screenshots = [
            ("offset-zero.png", 0.0, "0 samples · 0.00 ms"),
            ("offset-positive.png", 0.25, "+12000 samples · +250.00 ms"),
            ("offset-negative.png", 0.75, "-12000 samples · -250.00 ms"),
        ];
        let root = std::env::var_os("TOYBOX_UI_SCREENSHOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/ui-screenshots"))
            .join("wave");
        std::fs::create_dir_all(&root).expect("screenshot directory should be writable");
        for (filename, offset, expected) in screenshots {
            editor.runtime.bridge_mut().state_mut().waveform_offset = offset;
            editor.runtime.refresh();
            let plan = editor.paint_plan().clone();
            assert!(plan.contains_text(expected));
            let mut capture = toybox::radiant_gui::bundled_offscreen_capture(
                Vector2::new(WINDOW_WIDTH as f32, WINDOW_HEIGHT as f32),
                DpiScale::ONE,
            )
            .expect("Radiant offscreen capture should be available");
            let pixels = capture.capture(&plan).expect("screenshot should render");
            image::save_buffer_with_format(
                root.join(filename),
                &pixels,
                WINDOW_WIDTH,
                WINDOW_HEIGHT,
                ColorType::Rgba8,
                ImageFormat::Png,
            )
            .expect("screenshot should be written");
        }
    }

    #[test]
    fn screenshot_renders_high_resolution_waveform() {
        let publication = Arc::new(WaveformPublication::new());
        let envelope = std::array::from_fn(|index| {
            let phase = index as f32 / (ENVELOPE_BINS.saturating_sub(1).max(1) as f32);
            let kick =
                (1.0 - (phase * 18.0).fract()).powi(3) * (-((phase - 0.12) * 34.0).powi(2)).exp();
            let body = (1.0 - phase).max(0.0).powi(2) * 0.18;
            let amplitude = (kick + body).min(1.0);
            crate::capture::EnvelopePoint {
                min: -amplitude * (0.92 - phase * 0.15).max(0.55),
                max: amplitude,
            }
        });
        publication.publish_envelope(
            SnapshotMode::Synced,
            WindowLength::FourBeats,
            Some(128.0),
            Some(4),
            22_500,
            &envelope,
        );
        publication.set_selected_window(WindowLength::FourBeats);
        publication.update_transport(crate::capture::TransportInfo {
            tempo_bpm: Some(128.0),
            song_pos_beats: Some(4.35),
            is_playing: true,
        });
        let mut editor = WaveEditor::new(publication);
        let plan = editor.paint_plan().clone();
        assert!(plan.contains_text("HELD · 4 beats"));
        let mut capture = toybox::radiant_gui::bundled_offscreen_capture(
            Vector2::new(WINDOW_WIDTH as f32, WINDOW_HEIGHT as f32),
            DpiScale::ONE,
        )
        .expect("Radiant offscreen capture should be available");
        let pixels = capture.capture(&plan).expect("screenshot should render");
        let root = std::env::var_os("TOYBOX_UI_SCREENSHOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/ui-screenshots"))
            .join("wave");
        std::fs::create_dir_all(&root).expect("screenshot directory should be writable");
        image::save_buffer_with_format(
            root.join("high-resolution-waveform.png"),
            &pixels,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            ColorType::Rgba8,
            ImageFormat::Png,
        )
        .expect("screenshot should be written");
    }

    #[test]
    fn screenshot_renders_open_window_dropdown() {
        let publication = Arc::new(WaveformPublication::new());
        let mut editor = WaveEditor::new(publication);
        let _ = editor.paint_plan();
        let trigger = Point::new(
            WINDOW_DROPDOWN_X + WINDOW_DROPDOWN_WIDTH * 0.5,
            WINDOW_DROPDOWN_TRIGGER_Y + WINDOW_DROPDOWN_TRIGGER_HEIGHT * 0.5,
        );
        editor.dispatch_event(Event::primary_press(trigger));
        editor.dispatch_event(Event::primary_release(trigger));
        assert!(editor.runtime.bridge().state().window_dropdown_open);
        let plan = editor.paint_plan().clone();
        for window in WindowLength::ALL {
            assert!(
                plan.contains_text(window.label()),
                "open dropdown should show {}",
                window.label()
            );
            let option_rects = plan
                .text_runs()
                .filter(|run| run.text.as_str() == window.label())
                .map(|run| run.rect)
                .collect::<Vec<_>>();
            assert!(
                option_rects.iter().any(|rect| {
                    rect.min.y >= WINDOW_HEADER_HEIGHT && rect.max.y <= WINDOW_HEIGHT as f32
                }),
                "option {} rectangles: {option_rects:?}",
                window.label()
            );
            for rect in option_rects {
                assert!(rect.min.x >= 0.0);
                assert!(rect.max.x <= WINDOW_WIDTH as f32);
                assert!(rect.max.y <= WINDOW_HEIGHT as f32);
            }
        }
        assert!(plan.contains_text("PortalSurfer"));
        assert!(plan.contains_text("WAVE"));
        assert!(plan.contains_text(WINDOW_VERSION_LABEL));
        assert!(!plan.contains_text("WAVE  /  BEAT-SYNCED WAVEFORM"));

        let mut capture = toybox::radiant_gui::bundled_offscreen_capture(
            Vector2::new(WINDOW_WIDTH as f32, WINDOW_HEIGHT as f32),
            DpiScale::ONE,
        )
        .expect("Radiant offscreen capture should be available");
        let pixels = capture.capture(&plan).expect("screenshot should render");
        let root = std::env::var_os("TOYBOX_UI_SCREENSHOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/ui-screenshots"))
            .join("wave");
        std::fs::create_dir_all(&root).expect("screenshot directory should be writable");
        image::save_buffer_with_format(
            root.join("open-window-dropdown.png"),
            &pixels,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            ColorType::Rgba8,
            ImageFormat::Png,
        )
        .expect("screenshot should be written");
    }

    #[test]
    fn screenshot_renders_partial_live_waveform() {
        let publication = Arc::new(WaveformPublication::new());
        let envelope = std::array::from_fn(|index| {
            let phase = index as f32 / (ENVELOPE_BINS.saturating_sub(1).max(1) as f32);
            let amplitude = (1.0 - phase).max(0.0) * 0.72;
            crate::capture::EnvelopePoint {
                min: -amplitude,
                max: amplitude,
            }
        });
        publication.publish_envelope(
            SnapshotMode::Synced,
            WindowLength::FourBeats,
            Some(124.0),
            Some(4),
            96_000,
            &envelope,
        );
        publication.publish_live_preview(
            crate::capture::TransportInfo {
                tempo_bpm: Some(124.0),
                song_pos_beats: Some(8.2),
                is_playing: true,
            },
            SnapshotMode::Synced,
            WindowLength::FourBeats,
            Some(124.0),
            Some(8),
            224,
            96_000,
            &envelope,
        );
        publication.set_selected_window(WindowLength::FourBeats);
        let mut editor = WaveEditor::new(publication);
        let plan = editor.paint_plan().clone();
        assert!(plan.contains_text("LIVE · 4 beats"));
        let mut capture = toybox::radiant_gui::bundled_offscreen_capture(
            Vector2::new(WINDOW_WIDTH as f32, WINDOW_HEIGHT as f32),
            DpiScale::ONE,
        )
        .expect("Radiant offscreen capture should be available");
        let pixels = capture.capture(&plan).expect("screenshot should render");
        let root = std::env::var_os("TOYBOX_UI_SCREENSHOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/ui-screenshots"))
            .join("wave");
        std::fs::create_dir_all(&root).expect("screenshot directory should be writable");
        image::save_buffer_with_format(
            root.join("partial-live-waveform.png"),
            &pixels,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            ColorType::Rgba8,
            ImageFormat::Png,
        )
        .expect("screenshot should be written");
    }

    #[test]
    fn screenshot_renders_multi_beat_live_waveform() {
        let publication = Arc::new(WaveformPublication::new());
        publication.set_selected_window(WindowLength::EightBeats);
        let envelope = std::array::from_fn(|index| {
            let phase = index as f32 / (ENVELOPE_BINS.saturating_sub(1).max(1) as f32);
            let amplitude = (1.0 - phase).max(0.0) * 0.68;
            crate::capture::EnvelopePoint {
                min: -amplitude,
                max: amplitude,
            }
        });
        publication.publish_envelope(
            SnapshotMode::Synced,
            WindowLength::EightBeats,
            Some(20.0),
            Some(0),
            4_608_000,
            &envelope,
        );
        publication.publish_live_preview(
            crate::capture::TransportInfo {
                tempo_bpm: Some(20.0),
                song_pos_beats: Some(8.5),
                is_playing: true,
            },
            SnapshotMode::Synced,
            WindowLength::EightBeats,
            Some(20.0),
            Some(8),
            768_000,
            4_608_000,
            &envelope,
        );
        let mut editor = WaveEditor::new(publication);
        let plan = editor.paint_plan().clone();
        assert!(plan.contains_text(WindowLength::EightBeats.label()));
        assert!(plan.contains_text("LIVE · 8 beats"));
        let mut capture = toybox::radiant_gui::bundled_offscreen_capture(
            Vector2::new(WINDOW_WIDTH as f32, WINDOW_HEIGHT as f32),
            DpiScale::ONE,
        )
        .expect("Radiant offscreen capture should be available");
        let pixels = capture.capture(&plan).expect("screenshot should render");
        let root = std::env::var_os("TOYBOX_UI_SCREENSHOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/ui-screenshots"))
            .join("wave");
        std::fs::create_dir_all(&root).expect("screenshot directory should be writable");
        image::save_buffer_with_format(
            root.join("multi-beat-live-waveform.png"),
            &pixels,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            ColorType::Rgba8,
            ImageFormat::Png,
        )
        .expect("screenshot should be written");
    }
}
