//! Retained Radiant editor for the beat-synced waveform viewer.

use std::sync::Arc;

use radiant::application::{
    DropdownOption, IntoView, dropdown_menu_overlay_below, dropdown_trigger,
};
use radiant::gui::automation::AutomationRole;
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
    ButtonMessage, ButtonWidget, PointerShieldMessage, Widget, WidgetCapabilities, WidgetCommon,
    WidgetInput, WidgetKey, WidgetOutput, WidgetSemantics, WidgetSizing,
};

use crate::capture::{
    ENVELOPE_BINS, SnapshotMode, WaveformPublication, WaveformView, WindowLength, current_phase,
    grid_x, window_phase,
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
const WINDOW_HELP_WIDTH: f32 = 306.0;
const WINDOW_HELP_HEIGHT: f32 = 160.0;
const WINDOW_HELP_RIGHT_INSET: f32 = 16.0;
const WINDOW_HEADER_BUTTON_TEXT_TOP_INSET: f32 = 3.4;
const WINDOW_HEADER_BUTTON_FONT_SIZE: f32 = 12.0;
const WINDOW_DROPDOWN_WIDGET_ID: u64 = 0x5741_5645_0000_0001;
const WINDOW_HELP_BUTTON_WIDGET_ID: u64 = 0x5741_5645_0000_0002;
const WINDOW_VERSION_LABEL: &str = env!("CARGO_PKG_VERSION");

const WINDOW_HELP_ROWS: [(&str, &str); 5] = [
    ("Tab / Shift + Tab", "Move focus"),
    ("Enter / Space", "Activate the focused control"),
    ("WINDOW menu", "Select a beat window"),
    ("Escape", "Dismiss the open menu or help"),
    ("Click outside", "Dismiss the open menu or help"),
];

#[derive(Clone)]
struct WaveformWidget {
    common: WidgetCommon,
    view: WaveformView,
}

impl WaveformWidget {
    fn new(view: WaveformView) -> Self {
        Self {
            common: WidgetCommon::new(
                1,
                WidgetSizing::new(Vector2::new(1.0, 1.0), Vector2::new(720.0, 420.0)),
            )
            .without_default_chrome(),
            view,
        }
    }

    fn status_text(&self) -> String {
        let live = self.showing_live_preview();
        let retained = self.showing_retained_preview();
        let mode = if retained {
            "RETAINED PREFIX"
        } else {
            match self.display_mode() {
                SnapshotMode::Empty => "WAITING",
                SnapshotMode::Synced => "SYNCED",
                SnapshotMode::UnsyncedTempo => "UNSYNCED POSITION",
                SnapshotMode::UnsyncedFallback => "UNSYNCED 500 MS",
            }
        };
        let tempo = self
            .display_tempo_bpm()
            .map(|tempo| format!("{tempo:.1} BPM"))
            .unwrap_or_else(|| "NO TEMPO".to_string());
        let window = self.displayed_window().label();
        let state = if live {
            "LIVE"
        } else if retained {
            if self.view.is_playing {
                "INVALIDATED"
            } else {
                "HELD"
            }
        } else if self.view.snapshot_revision > 0 && !self.view.is_playing {
            "HELD"
        } else {
            "WAITING FOR WINDOW"
        };
        let progress = if live {
            format!(
                "{}/{} samples",
                self.view.live_sample_count, self.view.target_sample_count
            )
        } else if retained {
            format!(
                "{}/{} samples (not completed)",
                self.view.display_sample_count, self.view.display_target_sample_count
            )
        } else if self.view.snapshot_revision > 0 {
            "complete".to_string()
        } else {
            "no completed frame".to_string()
        };
        format!("{state}   ·   {window}   ·   {mode}   ·   {tempo}   ·   {progress}")
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

    fn display_mode(&self) -> SnapshotMode {
        if self.showing_live_preview() {
            self.view.live_mode
        } else if self.showing_retained_preview() {
            self.view.display_mode
        } else {
            self.view.snapshot_mode
        }
    }

    fn display_tempo_bpm(&self) -> Option<f32> {
        if self.showing_live_preview() {
            self.view.live_tempo_bpm.or(self.view.current_tempo_bpm)
        } else if self.showing_retained_preview() {
            self.view.display_tempo_bpm
        } else {
            self.view.snapshot_tempo_bpm.or(self.view.current_tempo_bpm)
        }
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

    fn footer_text(&self) -> String {
        if self.showing_live_preview() {
            let progress = self.view.live_sample_count as f32
                / self.view.target_sample_count.max(1) as f32
                * 100.0;
            format!(
                "LIVE · {} · {:.0}% captured · prior envelope stays visible until replaced; preview updates at 60 Hz.",
                self.displayed_window().label(),
                progress.clamp(0.0, 100.0),
            )
        } else if self.showing_retained_preview() {
            let state = if self.view.is_playing {
                "INVALIDATED"
            } else {
                "HELD"
            };
            format!(
                "{state} · last live prefix retained; waiting for a new complete {}. It is not a completed frame.",
                self.displayed_window().label()
            )
        } else if self.view.snapshot_revision > 0 && !self.view.is_playing {
            match self.view.snapshot_mode {
                SnapshotMode::Synced => {
                    format!(
                        "HELD · latest completed {}; grid remains locked to host beats.",
                        self.displayed_window().label()
                    )
                }
                SnapshotMode::UnsyncedTempo => {
                    format!(
                        "HELD · latest completed rolling {}; musical position is unavailable.",
                        self.displayed_window().label()
                    )
                }
                SnapshotMode::UnsyncedFallback => {
                    format!(
                        "HELD · latest completed {} selection with bounded 500 ms fallback; transport is unavailable.",
                        self.displayed_window().label()
                    )
                }
                SnapshotMode::Empty => format!(
                    "WAITING FOR WINDOW · no completed {} yet.",
                    self.displayed_window().label()
                ),
            }
        } else if self.view.is_playing {
            format!(
                "WAITING FOR WINDOW · showing the last completed {} while the next capture starts.",
                self.displayed_window().label()
            )
        } else {
            format!(
                "WAITING FOR WINDOW · start playback to capture the first complete {}.",
                self.displayed_window().label()
            )
        }
    }
}

impl Widget for WaveformWidget {
    fn common(&self) -> &WidgetCommon {
        &self.common
    }

    fn common_mut(&mut self) -> &mut WidgetCommon {
        &mut self.common
    }

    fn handle_input(&mut self, _bounds: Rect, _input: WidgetInput) -> Option<WidgetOutput> {
        None
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
        let height = bounds.height().max(1.0);
        let header_height = 34.0_f32.min(height * 0.2);
        let footer_height = 28.0_f32.min(height * 0.12);
        let chart = Rect::from_xy_size(
            bounds.min.x + 16.0,
            bounds.min.y + header_height,
            (width - 32.0).max(1.0),
            (height - header_height - footer_height - 16.0).max(1.0),
        );

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
            color: theme.border_emphasis,
            width: 1.0,
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
        let (sample_count, target_sample_count, envelope) = if show_live {
            (
                self.view.live_sample_count,
                self.view.target_sample_count,
                &self.view.live_envelope,
            )
        } else if show_retained {
            (
                self.view.display_sample_count,
                self.view.display_target_sample_count,
                &self.view.display_envelope,
            )
        } else {
            (
                self.view.sample_count,
                self.view.sample_count,
                &self.view.envelope,
            )
        };
        if sample_count > 0 {
            let captured_bins = captured_prefix_bins(sample_count, target_sample_count);
            // Keep the prior complete envelope under the unfilled live suffix.
            // The live or retained lane still owns the displayed metadata and
            // grid; this is only the visual continuity buffer while its
            // envelope is replaced.
            let preserve_completed_tail = (show_live || show_retained)
                && self.view.snapshot_revision > 0
                && self.view.sample_count > 0;
            let drawn_bins = if preserve_completed_tail {
                ENVELOPE_BINS
            } else {
                captured_bins
            };
            let denominator = ENVELOPE_BINS.saturating_sub(1).max(1) as f32;
            let mut top_contour = Vec::with_capacity(drawn_bins);
            let mut bottom_contour = Vec::with_capacity(drawn_bins);
            let mut peak_rects = Vec::new();
            for (index, live_point) in envelope.iter().take(drawn_bins).enumerate() {
                let point = if preserve_completed_tail && index >= captured_bins {
                    self.view.envelope[index]
                } else {
                    *live_point
                };
                let min = point.min.clamp(-1.0, 1.0);
                let max = point.max.clamp(-1.0, 1.0);
                let x = chart.min.x + index as f32 / denominator * chart.width();
                let top = chart.min.y + (1.0 - max) * chart.height() * 0.5;
                let bottom = chart.min.y + (1.0 - min) * chart.height() * 0.5;
                top_contour.push(Point::new(x, top));
                bottom_contour.push(Point::new(x, bottom));
                if max.abs().max(min.abs()) > 0.85 {
                    peak_rects.push(Rect::from_xy_size(
                        x.clamp(chart.min.x, chart.max.x) - 0.75,
                        top,
                        1.5,
                        (bottom - top).max(1.0),
                    ));
                }
            }

            let mut fill_points = Vec::with_capacity(top_contour.len() + bottom_contour.len());
            fill_points.extend(top_contour.iter().copied());
            fill_points.extend(bottom_contour.iter().rev().copied());
            let waveform_color = if show_live {
                theme.highlight_blue
            } else if show_retained {
                theme.highlight_orange
            } else {
                theme.accent_mint
            };
            primitives.push(PaintPrimitive::FillPolygon(PaintFillPolygon {
                widget_id: id,
                points: fill_points.into(),
                color: waveform_color.with_alpha(if show_live { 32 } else { 64 }),
            }));
            primitives.push(PaintPrimitive::StrokePolyline(PaintStrokePolyline {
                widget_id: id,
                points: top_contour.into(),
                color: waveform_color.with_alpha(if show_live { 176 } else { 232 }),
                width: 1.0,
            }));
            primitives.push(PaintPrimitive::StrokePolyline(PaintStrokePolyline {
                widget_id: id,
                points: bottom_contour.into(),
                color: waveform_color.with_alpha(if show_live { 128 } else { 184 }),
                width: 1.0,
            }));
            if !peak_rects.is_empty() {
                primitives.push(PaintPrimitive::FillRectBatch(PaintFillRectBatch {
                    widget_id: id,
                    rects: peak_rects.into(),
                    color: if show_live {
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

        primitives.push(PaintPrimitive::Text(PaintTextRun {
            widget_id: id,
            text: PaintText::from(self.footer_text()),
            rect: Rect::from_xy_size(
                bounds.min.x + 16.0,
                bounds.max.y - footer_height,
                width - 32.0,
                footer_height,
            ),
            font_size: 10.0,
            baseline: Some(11.0),
            color: theme.text_muted,
            align: PaintTextAlign::Left,
            wrap: radiant::widgets::TextWrap::None,
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

        let key_width = 128.0;
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditorMessage {
    ToggleWindowDropdown,
    SelectWindow(WindowLength),
    ToggleHelp,
    DismissTransient,
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
        custom_widget_direct(WaveformWidget::new(state.view)).fill(),
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
    }
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
    use crate::capture::EnvelopePoint;
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
        assert!(widget.status_text().contains("WAITING FOR WINDOW"));
        assert!(
            widget
                .status_text()
                .contains(WindowLength::FourBeats.label())
        );
        assert!(widget.footer_text().contains("last completed"));

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
        assert!(widget.status_text().contains("INVALIDATED"));
        assert!(widget.status_text().contains("RETAINED PREFIX"));
        assert!(widget.footer_text().contains("not a completed frame"));

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
            is_playing: false,
            ..WaveformView::default()
        };

        let widget = WaveformWidget::new(view);
        assert!(!widget.showing_live_preview());
        assert!(widget.showing_retained_preview());
        assert_eq!(widget.displayed_window(), WindowLength::TwoBeats);
        assert_eq!(widget.display_tempo_bpm(), Some(118.0));
        assert!(widget.status_text().contains("HELD"));
        assert!(widget.status_text().contains("RETAINED PREFIX"));
        assert!(
            widget
                .status_text()
                .contains("512/1024 samples (not completed)")
        );
        assert!(widget.footer_text().contains("not a completed frame"));

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

        click(&mut editor, center(trigger));
        assert!(editor.runtime.bridge().state().window_dropdown_open);
        assert!(!editor.runtime.bridge().state().help_open);
        assert_eq!(
            editor.runtime.focused_widget(),
            Some(WINDOW_DROPDOWN_WIDGET_ID)
        );
        let plan = editor.paint_plan().clone();
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
        assert!(editor.paint_plan().contains_text(selected.label()));
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
}

#[cfg(all(test, feature = "screenshot-test"))]
mod screenshot_tests {
    use super::*;
    use image::{ColorType, ImageFormat};
    use radiant::theme::DpiScale;
    use std::path::PathBuf;
    use toybox::radiant_gui::RadiantEditor;

    #[test]
    fn screenshot_renders_initial_ui() {
        let publication = Arc::new(WaveformPublication::new());
        let mut editor = WaveEditor::new(publication);
        let plan = editor.paint_plan().clone();
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
