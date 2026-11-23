//! The on-screen metrics panel: lines of text turned into rectangles for the renderer.
//!
//! Text is drawn with an 8 by 8 pixel font (the public-domain `font8x8` table). Every row of
//! every glyph becomes one rectangle per run of lit pixels, so a letter costs a handful of
//! rectangles. The renderer draws the rectangles as colored, blended quads in window pixels,
//! and knows nothing about fonts.

use font8x8::legacy::BASIC_LEGACY;
use vox_render::{OverlayRect, RenderStats};

use crate::metrics::{LOW_0_1_PERCENT_LABEL, LOW_1_PERCENT_LABEL, Summary, millis};
use crate::streaming::StreamStats;

/// Pixels along each side of a font cell.
const GLYPH_PIXELS: usize = 8;
/// The character drawn for anything the font lacks.
const MISSING_GLYPH: char = '?';
/// Window pixels per font pixel.
const TEXT_SCALE: f32 = 2.0;
/// Empty font pixels between two lines of text.
const LINE_GAP_PIXELS: usize = 2;
/// Distance of the panel from the top-left corner of the window, in window pixels.
const PANEL_MARGIN: f32 = 10.0;
/// Space between the panel's edge and its text, in window pixels.
const PANEL_PADDING: f32 = 8.0;
/// The panel's background: black, mostly see-through, so the game stays visible behind it.
const PANEL_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.55];
/// The text: opaque white.
const TEXT_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Spaces between the label column and the values.
const LABEL_GAP: usize = 1;
/// Width of the label column, so the values of all lines start at the same place: the longest
/// label (the 0.1% low, which is the longest of the labels in the panel) plus [`LABEL_GAP`].
const LABEL_WIDTH: usize = LOW_0_1_PERCENT_LABEL.len() + LABEL_GAP;

/// The font cell of `character`. Printable basic Latin has its own glyph; control characters
/// and everything outside basic Latin show as [`MISSING_GLYPH`], so a stray character is
/// visible instead of silently blank.
fn glyph(character: char) -> [u8; GLYPH_PIXELS] {
    let drawable = character.is_ascii() && !character.is_control();
    let shown = if drawable { character } else { MISSING_GLYPH };
    // `shown` is ASCII, so its code is below the 128 entries of the table.
    BASIC_LEGACY[shown as usize]
}

/// The runs of lit pixels in one glyph row as `(first pixel, length)`. In the font table the
/// lowest bit is the leftmost pixel.
fn row_runs(bits: u8) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut run_start: Option<usize> = None;
    // One step past the last pixel closes a run that reaches the edge of the cell.
    for x in 0..=GLYPH_PIXELS {
        let lit = x < GLYPH_PIXELS && (bits >> x) & 1 == 1;
        match (lit, run_start) {
            (true, None) => run_start = Some(x),
            (false, Some(start)) => {
                runs.push((start, x - start));
                run_start = None;
            }
            _ => {}
        }
    }
    runs
}

/// A rectangle in window pixels with a color.
fn rect(x: f32, y: f32, width: f32, height: f32, color: [f32; 4]) -> OverlayRect {
    OverlayRect {
        x,
        y,
        width,
        height,
        color,
    }
}

/// The rectangles of one line of text whose top-left corner is at `(x, y)` window pixels.
fn text_rects(text: &str, x: f32, y: f32) -> Vec<OverlayRect> {
    let mut rects = Vec::new();
    for (column, character) in text.chars().enumerate() {
        let cell_x = x + (column * GLYPH_PIXELS) as f32 * TEXT_SCALE;
        for (row, bits) in glyph(character).into_iter().enumerate() {
            let row_y = y + row as f32 * TEXT_SCALE;
            for (start, length) in row_runs(bits) {
                rects.push(rect(
                    cell_x + start as f32 * TEXT_SCALE,
                    row_y,
                    length as f32 * TEXT_SCALE,
                    TEXT_SCALE,
                    TEXT_COLOR,
                ));
            }
        }
    }
    rects
}

/// The overlay of a panel with these lines of text: a translucent background rectangle first,
/// then the text on top, at the top-left corner of the window. No lines give no rectangles.
pub fn panel(lines: &[String]) -> Vec<OverlayRect> {
    let Some(longest) = lines.iter().map(|line| line.chars().count()).max() else {
        return Vec::new();
    };
    let line_pixels = GLYPH_PIXELS + LINE_GAP_PIXELS;
    let text_width = (longest * GLYPH_PIXELS) as f32 * TEXT_SCALE;
    // The last line has no gap below it.
    let text_height = (lines.len() * line_pixels - LINE_GAP_PIXELS) as f32 * TEXT_SCALE;

    let mut rects = vec![rect(
        PANEL_MARGIN,
        PANEL_MARGIN,
        text_width + 2.0 * PANEL_PADDING,
        text_height + 2.0 * PANEL_PADDING,
        PANEL_COLOR,
    )];
    let text_x = PANEL_MARGIN + PANEL_PADDING;
    let text_y = PANEL_MARGIN + PANEL_PADDING;
    for (index, line) in lines.iter().enumerate() {
        let line_y = text_y + (index * line_pixels) as f32 * TEXT_SCALE;
        rects.extend(text_rects(line, text_x, line_y));
    }
    rects
}

/// Length of each arm of the crosshair, from its centre to its tip, in window pixels.
const CROSSHAIR_ARM: f32 = 10.0;
/// Thickness of the crosshair's bars, in window pixels. Even, so the bars sit centred on a
/// pixel border and look the same on both sides.
const CROSSHAIR_THICKNESS: f32 = 2.0;
/// Bars in one layer of the crosshair: one horizontal and one vertical.
const CROSSHAIR_BARS_PER_LAYER: usize = 2;
/// Layers of the crosshair: the dark outline, then the white bars over it.
const CROSSHAIR_LAYERS: usize = 2;
/// Extra pixels of dark backing around the white bars, so the crosshair stays visible against
/// bright sky and snow as well as against dark terrain.
const CROSSHAIR_OUTLINE: f32 = 1.0;

/// The crosshair for a window of `width` by `height` pixels: a white plus sign with a dark
/// outline, centred in the window. It marks where the player's reach starts, the block that
/// breaking and placing act on.
pub fn crosshair(width: u32, height: u32) -> Vec<OverlayRect> {
    let centre_x = width as f32 / 2.0;
    let centre_y = height as f32 / 2.0;
    // A bar of `length` by `thickness` centred on the window centre, grown by `grow` all round.
    let bar = |length: f32, thickness: f32, grow: f32, color: [f32; 4]| {
        let (w, h) = (length + 2.0 * grow, thickness + 2.0 * grow);
        rect(centre_x - w / 2.0, centre_y - h / 2.0, w, h, color)
    };
    let length = 2.0 * CROSSHAIR_ARM;
    let layers: [([f32; 4], f32); CROSSHAIR_LAYERS] =
        [(PANEL_COLOR, CROSSHAIR_OUTLINE), (TEXT_COLOR, 0.0)];
    let mut rects = Vec::with_capacity(CROSSHAIR_LAYERS * CROSSHAIR_BARS_PER_LAYER);
    for (color, grow) in layers {
        rects.push(bar(length, CROSSHAIR_THICKNESS, grow, color));
        rects.push(bar(CROSSHAIR_THICKNESS, length, grow, color));
    }
    rects
}

/// One line of the panel: a label, padded to a fixed column, and its value.
fn line(label: &str, value: impl std::fmt::Display) -> String {
    format!("{label:<LABEL_WIDTH$}{value}")
}

/// The text of the panel: the frame figures, what is drawn, how far streaming has come, and
/// how to use the keys.
/// `summary` is `None` until the first frames have been measured.
pub fn lines(
    summary: Option<&Summary>,
    stats: &RenderStats,
    stream: &StreamStats,
    frame_limit: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    match summary {
        Some(summary) => {
            let average_frame = millis(summary.covered) / summary.frames as f64;
            lines.push(line("FPS", format!("{:.0}", summary.average_fps)));
            lines.push(line(
                LOW_1_PERCENT_LABEL,
                format!("{:.0}", summary.low_1_percent),
            ));
            lines.push(line(
                LOW_0_1_PERCENT_LABEL,
                format!("{:.0}", summary.low_0_1_percent),
            ));
            lines.push(line(
                "Frame",
                format!(
                    "{:.2} ms avg, {:.1} ms worst",
                    average_frame,
                    millis(summary.worst_interval)
                ),
            ));
            lines.push(line(
                "Work",
                format!(
                    "{:.2} ms avg, {:.1} ms worst",
                    millis(summary.average_work),
                    millis(summary.worst_work)
                ),
            ));
            // The GPU time is an upper bound (decision 0008), so the panel says so.
            lines.push(line(
                "GPU",
                summary.average_gpu.map_or_else(
                    || "n/a".to_owned(),
                    |gpu| format!("<= {:.2} ms", millis(gpu)),
                ),
            ));
            // The counts are only meaningful once frames have been drawn.
            lines.push(line(
                "Drawn",
                format!(
                    "{} chunks, {} triangles",
                    stats.chunks_drawn, stats.triangles
                ),
            ));
        }
        None => lines.push(line("FPS", "measuring...")),
    }
    lines.push(line(
        "Chunks",
        format!("loaded {}  queued {}", stream.loaded, stream.queued),
    ));
    lines.push(line("Limit", if frame_limit { "on" } else { "off" }));
    lines.push(line("Keys", "L limit, F3 panel"));
    lines.push(line("Edit", "click break, F place"));
    lines
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_crosshair_is_centred_and_symmetric() {
        for (width, height) in [(1280_u32, 720_u32), (801, 601)] {
            let rects = crosshair(width, height);
            assert_eq!(rects.len(), 4);
            for r in &rects {
                assert_eq!(r.x + r.width / 2.0, width as f32 / 2.0);
                assert_eq!(r.y + r.height / 2.0, height as f32 / 2.0);
            }
        }
    }

    #[test]
    fn the_crosshair_has_a_dark_outline_below_white_bars() {
        let rects = crosshair(1000, 1000);
        // The outline bars come first, so the white ones are drawn over them, and are bigger.
        assert_eq!(rects[0].color, PANEL_COLOR);
        assert_eq!(rects[2].color, TEXT_COLOR);
        assert!(rects[0].width > rects[2].width);
        assert_eq!(rects[2].width, 2.0 * CROSSHAIR_ARM);
        assert_eq!(rects[2].height, CROSSHAIR_THICKNESS);
        assert_eq!(rects[3].width, CROSSHAIR_THICKNESS);
        assert_eq!(rects[3].height, 2.0 * CROSSHAIR_ARM);
    }

    #[test]
    fn a_zero_size_window_gives_finite_rectangles() {
        for r in crosshair(0, 0) {
            assert!([r.x, r.y, r.width, r.height].iter().all(|v| v.is_finite()));
        }
    }

    const MS: Duration = Duration::from_millis(1);

    fn summary() -> Summary {
        Summary {
            frames: 100,
            covered: 500 * MS,
            average_fps: 200.0,
            low_1_percent: 150.0,
            low_0_1_percent: 100.0,
            worst_interval: 10 * MS,
            average_work: MS,
            worst_work: 4 * MS,
            average_gpu: Some(2 * MS),
        }
    }

    #[test]
    fn runs_are_found_in_a_row_of_pixels() {
        assert_eq!(row_runs(0x00), vec![]);
        assert_eq!(row_runs(0xFF), vec![(0, 8)]);
        // The top row of the letter F: the seven lowest bits, starting at the left edge.
        assert_eq!(row_runs(0x7F), vec![(0, 7)]);
        // The second row of the F: a stem two pixels wide on the left, a lone pixel on the right.
        assert_eq!(row_runs(0x46), vec![(1, 2), (6, 1)]);
        // The two outer pixels, which the loop has to close at both ends of the cell.
        assert_eq!(row_runs(0x81), vec![(0, 1), (7, 1)]);
    }

    #[test]
    fn a_space_has_no_rectangles_and_a_letter_has_some() {
        assert!(text_rects(" ", 0.0, 0.0).is_empty());
        assert!(!text_rects("A", 0.0, 0.0).is_empty());
    }

    #[test]
    fn the_rectangles_of_a_letter_stay_inside_its_cell() {
        let cell = GLYPH_PIXELS as f32 * TEXT_SCALE;
        for r in text_rects("W", 100.0, 50.0) {
            assert!(r.x >= 100.0 && r.x + r.width <= 100.0 + cell, "{r:?}");
            assert!(r.y >= 50.0 && r.y + r.height <= 50.0 + cell, "{r:?}");
            assert!(r.width > 0.0 && r.height > 0.0);
        }
    }

    #[test]
    fn the_first_row_of_f_is_one_wide_rectangle_at_the_top_left() {
        let rects = text_rects("F", 0.0, 0.0);
        let top = &rects[0];
        assert_eq!((top.x, top.y), (0.0, 0.0));
        assert_eq!((top.width, top.height), (7.0 * TEXT_SCALE, TEXT_SCALE));
    }

    #[test]
    fn the_next_character_starts_one_cell_further_right() {
        let single = text_rects("F", 0.0, 0.0);
        let double = text_rects("FF", 0.0, 0.0);
        assert_eq!(double.len(), single.len() * 2);
        let shift = GLYPH_PIXELS as f32 * TEXT_SCALE;
        assert_eq!(double[single.len()].x, single[0].x + shift);
    }

    #[test]
    fn characters_the_font_lacks_show_as_a_question_mark() {
        assert_eq!(text_rects("\u{00e9}", 0.0, 0.0), text_rects("?", 0.0, 0.0));
        assert_eq!(text_rects("\u{1F600}", 0.0, 0.0), text_rects("?", 0.0, 0.0));
    }

    #[test]
    fn no_lines_make_no_panel() {
        assert!(panel(&[]).is_empty());
    }

    #[test]
    fn the_panel_starts_with_a_translucent_background_that_fits_the_text() {
        let rects = panel(&["AB".to_owned(), "ABCD".to_owned()]);
        let background = rects[0];
        assert_eq!(background.color, PANEL_COLOR);
        assert!(background.color[3] > 0.0 && background.color[3] < 1.0);
        assert_eq!((background.x, background.y), (PANEL_MARGIN, PANEL_MARGIN));
        // Four characters wide (the longest line), two lines high, plus the padding.
        let width = 4.0 * GLYPH_PIXELS as f32 * TEXT_SCALE + 2.0 * PANEL_PADDING;
        let height = (2 * (GLYPH_PIXELS + LINE_GAP_PIXELS) - LINE_GAP_PIXELS) as f32 * TEXT_SCALE
            + 2.0 * PANEL_PADDING;
        assert_eq!((background.width, background.height), (width, height));
        for r in &rects[1..] {
            assert_eq!(r.color, TEXT_COLOR);
            assert!(r.x >= background.x && r.x + r.width <= background.x + background.width);
            assert!(r.y >= background.y && r.y + r.height <= background.y + background.height);
        }
    }

    #[test]
    fn the_second_line_sits_below_the_first() {
        let one = panel(&["F".to_owned()]);
        let two = panel(&["F".to_owned(), "F".to_owned()]);
        let per_line = one.len() - 1;
        let first = two[1];
        let second = two[1 + per_line];
        let line_step = (GLYPH_PIXELS + LINE_GAP_PIXELS) as f32 * TEXT_SCALE;
        assert_eq!(second.y, first.y + line_step);
        assert_eq!(second.x, first.x);
    }

    #[test]
    fn the_lines_show_the_figures() {
        let text = lines(
            Some(&summary()),
            &RenderStats::default(),
            &StreamStats::default(),
            false,
        )
        .join("\n");
        assert!(text.contains("FPS      200"), "{text}");
        assert!(text.contains("1% low   150"), "{text}");
        assert!(text.contains("0.1% low 100"), "{text}");
        assert!(text.contains("5.00 ms avg, 10.0 ms worst"), "{text}");
        assert!(text.contains("GPU      <= 2.00 ms"), "{text}");
        assert!(text.contains("Limit    off"), "{text}");
    }

    #[test]
    fn an_unknown_gpu_time_shows_as_not_available() {
        let mut figures = summary();
        figures.average_gpu = None;
        let text = lines(
            Some(&figures),
            &RenderStats::default(),
            &StreamStats::default(),
            true,
        )
        .join("\n");
        assert!(text.contains("GPU      n/a"), "{text}");
        assert!(text.contains("Limit    on"), "{text}");
    }

    #[test]
    fn before_the_first_measurement_the_panel_says_so_and_shows_no_counts() {
        let text = lines(
            None,
            &RenderStats::default(),
            &StreamStats::default(),
            false,
        )
        .join("\n");
        assert!(text.contains("measuring"), "{text}");
        assert!(!text.contains("Drawn"), "{text}");
    }

    #[test]
    fn the_drawn_counts_come_from_the_render_stats() {
        let stats = RenderStats {
            chunks_drawn: 289,
            triangles: 1_014_652,
            ..RenderStats::default()
        };
        let text = lines(Some(&summary()), &stats, &StreamStats::default(), false).join("\n");
        assert!(text.contains("289 chunks, 1014652 triangles"), "{text}");
    }

    #[test]
    fn the_streaming_progress_has_a_line_before_the_first_measurement_too() {
        let stream = StreamStats {
            loaded: 120,
            queued: 77,
        };
        for summary in [Some(summary()), None] {
            let text = lines(summary.as_ref(), &RenderStats::default(), &stream, false).join("\n");
            assert!(text.contains("Chunks   loaded 120  queued 77"), "{text}");
        }
    }

    #[test]
    fn the_keys_have_a_line_of_their_own() {
        let text = lines(
            Some(&summary()),
            &RenderStats::default(),
            &StreamStats::default(),
            false,
        )
        .join("\n");
        assert!(text.contains("Keys     L limit, F3 panel"), "{text}");
        assert!(!text.contains("Limit    off (L"), "{text}");
    }

    #[test]
    fn control_characters_and_non_ascii_show_as_a_question_mark() {
        let question_mark = text_rects("?", 0.0, 0.0);
        assert_eq!(text_rects("\u{1}", 0.0, 0.0), question_mark);
        assert_eq!(text_rects("\u{7f}", 0.0, 0.0), question_mark);
        assert_eq!(text_rects("\u{e9}", 0.0, 0.0), question_mark);
    }

    #[test]
    fn every_line_is_plain_ascii_the_font_can_draw() {
        let text = lines(
            Some(&summary()),
            &RenderStats::default(),
            &StreamStats::default(),
            false,
        )
        .join("\n");
        assert!(text.is_ascii(), "{text}");
    }
}
