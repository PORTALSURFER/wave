//! Retained Radiant editor for the beat-synced waveform viewer.

use std::sync::Arc;

use radiant::application::{
    DropdownOption, IntoView, dropdown_menu_overlay_below, dropdown_trigger,
};
use radiant::gui::types::{Point, Rect, Vector2};
use radiant::layout::LayoutOutput;
use radiant::prelude::{column, custom_widget_direct, row, stack, text};
use radiant::runtime::{
    DeclarativeSurfaceRuntime, Event, PaintFillPolygon, PaintFillRect, PaintFillRectBatch,
    PaintPrimitive, PaintStrokePolyline, PaintStrokeRect, PaintText, PaintTextAlign, PaintTextRun,
    SurfacePaintPlan, UiSurface,
};
use radiant::theme::ThemeTokens;
use radiant::widgets::{Widget, WidgetCommon, WidgetInput, WidgetKey, WidgetOutput, WidgetSizing};

use crate::capture::{
    ENVELOPE_BINS, SnapshotMode, WaveformPublication, WaveformView, WindowLength, current_phase,
    grid_x, window_phase,
};

/// Preferred logical width of the embedded editor.
pub const WINDOW_WIDTH: u32 = 960;
/// Preferred logical height of the embedded editor.
pub const WINDOW_HEIGHT: u32 = 600;

const WINDOW_DROPDOWN_WIDTH: f32 = 236.0;
const WINDOW_HEADER_HEIGHT: f32 = 32.0;
const WINDOW_DROPDOWN_TRIGGER_Y: f32 = 4.0;
const WINDOW_DROPDOWN_TRIGGER_HEIGHT: f32 = 24.0;
const WINDOW_DROPDOWN_GAP: f32 = 4.0;
const WINDOW_DROPDOWN_MENU_WIDTH: f32 = 260.0;
const WINDOW_DROPDOWN_MENU_X: f32 = WINDOW_WIDTH as f32 - WINDOW_DROPDOWN_MENU_WIDTH - 16.0;

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

struct EditorState {
    publication: Arc<WaveformPublication>,
    view: WaveformView,
    selected_window: WindowLength,
    window_dropdown_open: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditorMessage {
    ToggleWindowDropdown,
    SelectWindow(WindowLength),
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
            .width(WINDOW_DROPDOWN_WIDTH)
            .height(WINDOW_DROPDOWN_TRIGGER_HEIGHT);
    let header_row = row([
        text("WAVE  /  BEAT-SYNCED WAVEFORM")
            .fill_width()
            .key("wave-header-title"),
        text("WINDOW").key("window-label"),
        window_dropdown,
    ])
    .fill_width()
    .height(WINDOW_HEADER_HEIGHT);
    let editor = column([
        header_row.key("wave-header"),
        custom_widget_direct(WaveformWidget::new(state.view)).fill(),
    ])
    .key("wave-editor")
    .fill();
    let surface = if state.window_dropdown_open {
        stack([
            editor,
            dropdown_menu_overlay_below(
                WINDOW_DROPDOWN_MENU_X,
                WINDOW_DROPDOWN_TRIGGER_Y,
                WINDOW_DROPDOWN_TRIGGER_HEIGHT,
                WINDOW_DROPDOWN_GAP,
                Some(WINDOW_DROPDOWN_MENU_WIDTH),
                window_options.collect(),
            ),
        ])
        .fill()
    } else {
        editor
    };
    Arc::new(surface.into_surface())
}

fn reduce_surface(state: &mut EditorState, message: EditorMessage) {
    match message {
        EditorMessage::ToggleWindowDropdown => {
            state.window_dropdown_open = !state.window_dropdown_open;
        }
        EditorMessage::SelectWindow(window) => {
            state.selected_window = window;
            state.window_dropdown_open = false;
            state.publication.set_selected_window(window);
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
}

impl toybox::radiant_gui::RadiantEditor for WaveEditor {
    fn resize(&mut self, width: u32, height: u32) {
        let _ = self.runtime.dispatch_event(Event::resize(Vector2::new(
            width.max(1) as f32,
            height.max(1) as f32,
        )));
    }

    fn dispatch_event(&mut self, event: Event) {
        let _ = self.runtime.dispatch_event(event);
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
        self.runtime.dispatch_event(Event::key_press(key)).is_some()
    }

    fn dispatch_character(&mut self, character: char) -> bool {
        self.runtime
            .dispatch_event(Event::character(character))
            .is_some()
    }

    fn cancel_text_entry(&mut self) -> bool {
        false
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
        let trigger = Point::new(WINDOW_WIDTH as f32 - 96.0, 16.0);
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
        assert_eq!(
            plan.text_labels()
                .filter(|label| *label == "WAVE  /  BEAT-SYNCED WAVEFORM")
                .count(),
            1,
            "the editor should expose one title from the outer header"
        );
        let title_rect = plan
            .first_text_rect("WAVE  /  BEAT-SYNCED WAVEFORM")
            .expect("outer header title should be painted");
        assert!(title_rect.max.y <= WINDOW_HEADER_HEIGHT);

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
