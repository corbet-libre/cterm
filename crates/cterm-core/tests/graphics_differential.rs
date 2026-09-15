//! Graphics-pipeline differential suite (compositor-free).
//!
//! The shared RGBA pipeline renders Sixel, Kitty graphics, and iTerm2 inline
//! images through one code path (`SixelImage` / `DecodedImage` -> `Screen`
//! images). This suite pins the compositor-free half of
//! `docs/parity.md` ("Differential protocol suite and semantic native image
//! assertions"):
//!
//! - deterministic fixtures with semantic RGBA assertions (dimensions, pixel
//!   values, palette application incl. DEC mode 1070, crop/scale geometry,
//!   alpha composition order, transient `N=1` handling),
//! - cross-protocol consistency vectors (same source through Sixel vs
//!   Kitty-raw vs PNG, with an explicit documented delta for lossy JPEG),
//! - hostile bounds that must fail closed within the documented budgets
//!   (Sixel `64 MiB` / `10_000 px`, Kitty `64 MiB` / `10_000 px`,
//!   `image_decode` `4096 px`).
//!
//! Still open (needs a compositor/GPU, not covered here): semantic native
//! image assertions on Cocoa/CoreGraphics, GTK/Cairo, and Win32/Direct2D
//! (pixel-level placement, scaling, and layering on real backends).

use base64::Engine as _;
use cterm_core::screen::{ImageLayer, Screen, ScreenConfig};
use cterm_core::sixel::{
    SixelDecoder, SixelDecoderConfig, DEFAULT_SIXEL_MAX_BYTES, MAX_SIXEL_COLORS,
    MAX_SIXEL_DIMENSION,
};
use cterm_core::{decode_image, Parser};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use image::ImageEncoder as _;
use std::io::Write;

fn make_screen() -> Screen {
    Screen::new(80, 24, ScreenConfig::default())
}

fn make_small_screen() -> Screen {
    Screen::new(10, 5, ScreenConfig::default())
}

fn kitty_apc(control: &str, payload: &[u8]) -> Vec<u8> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    format!("\x1b_G{control};{encoded}\x1b\\").into_bytes()
}

fn kitty_control_only(control: &str) -> Vec<u8> {
    format!("\x1b_G{control}\x1b\\").into_bytes()
}

fn iterm2_inline(payload: &[u8], params: &str) -> Vec<u8> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    format!("\x1b]1337;File={params}:{encoded}\x07").into_bytes()
}

fn put_all(decoder: &mut SixelDecoder, bytes: &[u8]) {
    for &byte in bytes {
        decoder.put(byte);
    }
}

fn encode_png_rgba(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    encoder
        .write_image(pixels, width, height, image::ExtendedColorType::Rgba8)
        .expect("deterministic PNG fixture must encode");
    buf
}

fn encode_jpeg_rgb(pixels_rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut buf = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 95);
    use image::ImageEncoder as _;
    encoder
        .write_image(pixels_rgb, width, height, image::ExtendedColorType::Rgb8)
        .expect("deterministic JPEG fixture must encode");
    buf
}

fn max_channel_delta(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Sixel decode fixtures
// ---------------------------------------------------------------------------

#[test]
fn sixel_rgb_primaries_decode_to_exact_rgba() {
    // P1=7 selects the 1:1 aspect so one sixel column is 1x6 px.
    let mut decoder = SixelDecoder::with_params(&[7]);
    put_all(&mut decoder, b"#1;2;100;0;0~#2;2;0;100;0~#3;2;0;0;100~");
    let image = decoder.finish().expect("three sixel columns");
    assert_eq!((image.width, image.height), (3, 6));
    assert_eq!(image.data.len(), 3 * 6 * 4);
    for row in 0..6 {
        assert_eq!(
            &image.data[(row * 3) * 4..(row * 3) * 4 + 4],
            &[255, 0, 0, 255],
            "row {row} col 0 is red"
        );
        assert_eq!(
            &image.data[(row * 3 + 1) * 4..(row * 3 + 1) * 4 + 4],
            &[0, 255, 0, 255],
            "row {row} col 1 is green"
        );
        assert_eq!(
            &image.data[(row * 3 + 2) * 4..(row * 3 + 2) * 4 + 4],
            &[0, 0, 255, 255],
            "row {row} col 2 is blue"
        );
    }
}

#[test]
fn sixel_repeat_expands_deterministically() {
    let mut decoder = SixelDecoder::with_params(&[7]);
    put_all(&mut decoder, b"#1;2;100;0;0!4~");
    let image = decoder.finish().unwrap();
    assert_eq!((image.width, image.height), (4, 6));
    assert!(image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .all(|px| px == &[255, 0, 0, 255]));
}

#[test]
fn sixel_carriage_return_and_line_feed_shape_geometry() {
    let mut decoder = SixelDecoder::with_params(&[7]);
    put_all(&mut decoder, b"~~~$??-~");
    let image = decoder.finish().unwrap();
    // Three columns on band 0, then CR + one blank column, LF to band 1,
    // then one column: extent is 3 wide and 12 tall.
    assert_eq!((image.width, image.height), (3, 12));
}

#[test]
fn sixel_raster_hint_establishes_background_geometry() {
    let mut decoder = SixelDecoder::with_params(&[7]);
    // Pan=2, Pad=3, Ph=4, Pv=5 => 12x10 background even before sixels.
    put_all(&mut decoder, b"\"2;3;4;5");
    let image = decoder.finish().unwrap();
    assert_eq!((image.width, image.height), (12, 10));
    assert_eq!(image.data.len(), 12 * 10 * 4);
}

#[test]
fn sixel_shared_palette_carries_red_across_images() {
    // Shared palettes (mode 1070 reset) are the default in this harness:
    // define red once, reuse the register in the next image.
    let mut screen = make_screen();
    let mut parser = Parser::new();
    parser.parse(
        &mut screen,
        b"\x1b[?1070l\x1bP7;1q#42;2;100;0;0~\x1b\\\x1bP7;1q#42~\x1b\\",
    );
    let images = screen.images();
    assert_eq!(images.len(), 2);
    assert_eq!(&images[0].data[..4], &[255, 0, 0, 255]);
    assert_eq!(&images[1].data[..4], &[255, 0, 0, 255]);
}

#[test]
fn sixel_private_palette_isolates_images() {
    let mut screen = make_screen();
    let mut parser = Parser::new();
    // Private palette: the second image must NOT see register 42 from the
    // first image, so it falls back to the VT340-compatible default.
    parser.parse(
        &mut screen,
        b"\x1b[?1070h\x1bP7;1q#42;2;100;0;0~\x1b\\\x1bP7;1q#42~\x1b\\",
    );
    let images = screen.images();
    assert_eq!(images.len(), 2);
    assert_eq!(&images[0].data[..4], &[255, 0, 0, 255]);
    let default_42 = SixelDecoder::default_palette(MAX_SIXEL_COLORS)[42];
    assert_eq!(
        &images[1].data[..4],
        &default_42,
        "private palette must not leak register 42"
    );
    assert_ne!(&images[1].data[..4], &[255, 0, 0, 255]);
}

#[test]
fn sixel_palette_index_overflow_clamps_fail_closed() {
    let mut decoder = SixelDecoder::with_params(&[7]);
    // Register 9999 is far beyond the 1024-entry palette: selection clamps
    // to the last entry instead of panicking.
    put_all(&mut decoder, b"#9999~");
    let result = decoder.finish_with_palette();
    assert!(!result.truncated);
    let image = result.image.unwrap();
    assert_eq!((image.width, image.height), (1, 6));
    let last = SixelDecoder::default_palette(MAX_SIXEL_COLORS)[MAX_SIXEL_COLORS - 1];
    assert_eq!(&image.data[..4], &last);
}

#[test]
fn sixel_budgets_match_documented_caps_and_truncate_deterministically() {
    assert_eq!(MAX_SIXEL_DIMENSION, 10_000);
    assert_eq!(DEFAULT_SIXEL_MAX_BYTES, 64 * 1024 * 1024);

    let config = SixelDecoderConfig {
        max_width: 8,
        max_height: 64,
        max_bytes: 8 * 64 * 4,
        ..SixelDecoderConfig::default()
    };
    let mut decoder = SixelDecoder::with_config(&[7], config);
    put_all(&mut decoder, b"!100~");
    let result = decoder.finish_with_palette();
    assert!(result.truncated);
    let image = result.image.unwrap();
    assert_eq!(image.width, 8);
    assert!(image.data.len() <= config.max_bytes);
}

// ---------------------------------------------------------------------------
// Kitty transfer fixtures (via the Parser APC path)
// ---------------------------------------------------------------------------

#[test]
fn kitty_raw_rgb24_expands_alpha_to_opaque() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    // 2x2 RGB: red, green / blue, white.
    let rgb = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
    parser.parse(&mut screen, &kitty_apc("a=T,f=24,s=2,v=2,i=101,C=1", &rgb));
    let images = screen.images();
    assert_eq!(images.len(), 1);
    assert_eq!((images[0].pixel_width, images[0].pixel_height), (2, 2));
    assert_eq!(
        images[0].data.as_slice(),
        &[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,]
    );
}

#[test]
fn kitty_raw_rgba32_preserves_alpha_channel() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    let rgba = [255, 0, 0, 128, 0, 255, 0, 0, 0, 0, 255, 255, 1, 2, 3, 4];
    parser.parse(&mut screen, &kitty_apc("a=T,f=32,s=2,v=2,i=102,C=1", &rgba));
    assert_eq!(screen.images()[0].data.as_slice(), &rgba);
}

#[test]
fn kitty_png_decodes_into_shared_rgba_pipeline() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    let rgba = [
        10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 1, 2, 3, 4,
    ];
    let png = encode_png_rgba(&rgba, 2, 2);
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    parser.parse(&mut screen, &kitty_apc("a=T,f=100,i=103,C=1", &png));
    let images = screen.images();
    assert_eq!(images.len(), 1);
    assert_eq!((images[0].pixel_width, images[0].pixel_height), (2, 2));
    assert_eq!(images[0].data.as_slice(), &rgba);
}

#[test]
fn kitty_zlib_payload_inflates_to_identical_rgba() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    let rgba = [9, 9, 9, 255, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&rgba).unwrap();
    let compressed = encoder.finish().unwrap();
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=2,v=2,o=z,i=104,C=1", &compressed),
    );
    assert_eq!(screen.images()[0].data.as_slice(), &rgba);
}

#[test]
fn kitty_truncated_zlib_fails_closed_without_image() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    let rgba = [1, 2, 3, 4, 5, 6, 7, 8];
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&rgba).unwrap();
    let mut compressed = encoder.finish().unwrap();
    // Keep only the zlib header: inflate must fail (not produce a partial
    // image). Truncating just the trailing Adler checksum is too lenient in
    // some flate2 versions, so cut deep.
    compressed.truncate(2);
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=2,v=1,o=z,i=105,C=1", &compressed),
    );
    assert!(screen.images().is_empty());
    let replies = screen.take_pending_responses();
    assert_eq!(replies.len(), 1);
    let reply = String::from_utf8_lossy(&replies[0]).into_owned();
    assert!(
        reply.contains("EINVAL") || reply.contains("ENODATA"),
        "truncated zlib must fail closed, got: {reply}"
    );
}

#[test]
fn kitty_chunked_reassembly_is_split_invariant() {
    let rgba = [11, 22, 33, 44, 55, 66, 77, 88];
    let encoded = base64::engine::general_purpose::STANDARD.encode(rgba);
    // Representative split points incl. chunk-boundary and odd splits.
    let splits = [1, 3, 4, 7, encoded.len() - 1];
    let mut reference: Option<Vec<u8>> = None;
    for split in splits {
        let mut screen = make_small_screen();
        let mut parser = Parser::new();
        let (first, second) = encoded.split_at(split);
        let head = format!("\x1b_Ga=t,f=32,s=2,v=1,i=110,m=1;{first}\x1b\\");
        let tail = format!("\x1b_Gm=0;{second}\x1b\\");
        parser.parse(&mut screen, head.as_bytes());
        assert!(
            screen.images().is_empty(),
            "split {split}: no image before final chunk"
        );
        parser.parse(&mut screen, tail.as_bytes());
        parser.parse(&mut screen, b"\x1b_Ga=p,i=110,p=1,C=1\x1b\\");
        let data = screen.images()[0].data.as_slice().to_vec();
        assert_eq!(data, rgba, "split at {split}");
        if let Some(expected) = &reference {
            assert_eq!(&data, expected);
        } else {
            reference = Some(data);
        }
    }
}

#[test]
fn kitty_byte_level_feeding_matches_whole_sequence() {
    let rgba = [200, 10, 20, 255];
    let whole = kitty_apc("a=T,f=32,s=1,v=1,i=111,C=1", &rgba);
    let mut full_screen = make_small_screen();
    Parser::new().parse(&mut full_screen, &whole);
    let expected = full_screen.images()[0].data.as_slice().to_vec();

    let mut split_screen = make_small_screen();
    let mut parser = Parser::new();
    for chunk in whole.chunks(1) {
        parser.parse(&mut split_screen, chunk);
    }
    assert_eq!(split_screen.images()[0].data.as_slice(), &expected);
}

#[test]
fn kitty_interleaved_delete_aborts_pending_upload_fail_closed() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    let rgba = [1, 2, 3, 255];
    let encoded = base64::engine::general_purpose::STANDARD.encode(rgba);
    let head = format!("\x1b_Ga=t,f=32,s=1,v=1,i=112,m=1;{}\x1b\\", &encoded[..4]);
    parser.parse(&mut screen, head.as_bytes());
    assert!(screen.images().is_empty());
    // A delete control arrives instead of the continuation chunk: the
    // pending upload is dropped and nothing is stored.
    parser.parse(&mut screen, b"\x1b_Ga=d,d=a\x1b\\");
    assert!(screen.images().is_empty());
    // The protocol recovers: a fresh single-shot upload still works.
    parser.parse(&mut screen, &kitty_apc("a=T,f=32,s=1,v=1,i=112,C=1", &rgba));
    assert_eq!(screen.images()[0].data.as_slice(), &rgba);
}

#[test]
fn kitty_invalid_base64_and_short_payload_fail_closed() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(&mut screen, b"\x1b_Ga=T,f=32,s=2,v=2,i=113;!!!!\x1b\\");
    assert!(screen.images().is_empty());
    let reply = String::from_utf8(screen.take_pending_responses().pop().unwrap()).unwrap();
    assert!(reply.contains("EINVAL"), "bad base64: {reply}");

    // Claims 2x2 RGBA (16 bytes) but delivers one pixel.
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=2,v=2,i=114,C=1", &[1, 2, 3, 4]),
    );
    assert_eq!(screen.images().len(), 0);
    let reply = String::from_utf8(screen.take_pending_responses().pop().unwrap()).unwrap();
    assert!(reply.contains("ENODATA"), "short payload: {reply}");
}

#[test]
fn kitty_crop_produces_pixel_exact_subrect() {
    let mut screen = make_small_screen();
    screen.set_cell_width_hint(1.0);
    screen.set_cell_height_hint(1.0);
    let mut parser = Parser::new();
    // 4x4 where pixel (x,y) = [x*60, y*60, 0, 255].
    let mut full = Vec::with_capacity(4 * 4 * 4);
    for y in 0..4 {
        for x in 0..4 {
            full.extend_from_slice(&[(x * 60) as u8, (y * 60) as u8, 0, 255]);
        }
    }
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=4,v=4,x=1,y=1,w=2,h=2,i=115,C=1", &full),
    );
    let images = screen.images();
    assert_eq!(images.len(), 1);
    assert_eq!((images[0].pixel_width, images[0].pixel_height), (2, 2));
    assert_eq!(
        images[0].data.as_slice(),
        &[60, 60, 0, 255, 120, 60, 0, 255, 60, 120, 0, 255, 120, 120, 0, 255]
    );
}

#[test]
fn kitty_single_axis_scale_preserves_aspect_and_solid_color() {
    let mut screen = make_small_screen();
    screen.set_cell_width_hint(4.0);
    screen.set_cell_height_hint(4.0);
    let mut parser = Parser::new();
    let red = [255, 0, 0, 255].repeat(4); // 2x2 solid red.
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=2,v=2,c=1,i=116,C=1", &red),
    );
    let images = screen.images();
    assert_eq!(images.len(), 1);
    // columns=1 at 4 px/cell => 4 px wide; rows derive to preserve aspect.
    assert_eq!((images[0].pixel_width, images[0].pixel_height), (4, 4));
    assert!(
        images[0]
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .all(|px| px == &[255, 0, 0, 255]),
        "solid source must stay solid through Triangle resampling"
    );
}

#[test]
fn kitty_cell_offsets_pad_with_transparent_pixels() {
    let mut screen = make_small_screen();
    screen.set_cell_width_hint(4.0);
    screen.set_cell_height_hint(4.0);
    let mut parser = Parser::new();
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=1,v=1,X=1,Y=2,i=117,C=1", &[9, 8, 7, 255]),
    );
    let images = screen.images();
    assert_eq!(images.len(), 1);
    assert_eq!((images[0].pixel_width, images[0].pixel_height), (2, 3));
    // Two transparent rows, then [transparent, source].
    assert_eq!(&images[0].data[..8], &[0; 8]);
    assert_eq!(&images[0].data[8..16], &[0; 8]);
    assert_eq!(&images[0].data[16..20], &[0, 0, 0, 0]);
    assert_eq!(&images[0].data[20..24], &[9, 8, 7, 255]);
}

// ---------------------------------------------------------------------------
// iTerm2 fixtures
// ---------------------------------------------------------------------------

#[test]
fn iterm2_png_inline_uses_native_dimensions_and_pixels() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    let rgba = [
        5, 6, 7, 255, 8, 9, 10, 255, 11, 12, 13, 255, 14, 15, 16, 255,
    ];
    let png = encode_png_rgba(&rgba, 2, 2);
    parser.parse(&mut screen, &iterm2_inline(&png, "inline=1"));
    let images = screen.images();
    assert_eq!(images.len(), 1);
    assert_eq!((images[0].pixel_width, images[0].pixel_height), (2, 2));
    assert_eq!(images[0].data.as_slice(), &rgba);
}

#[test]
fn iterm2_gif_first_frame_decodes_to_rgba() {
    // Minimal 1x1 GIF89a (transparent). Well-known deterministic blob.
    let gif = base64::engine::general_purpose::STANDARD
        .decode("R0lGODdhAQABAIAAAP///////ywAAAAAAQABAAACAkQBADs=")
        .unwrap();
    assert!(gif.starts_with(b"GIF87a") || gif.starts_with(b"GIF89a"));
    let decoded = decode_image(&gif).expect("fixture GIF must decode");
    assert_eq!((decoded.width, decoded.height), (1, 1));
    assert_eq!(decoded.data.len(), 4);

    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(&mut screen, &iterm2_inline(&gif, "inline=1"));
    assert_eq!(screen.images().len(), 1);
    assert_eq!(
        (
            screen.images()[0].pixel_width,
            screen.images()[0].pixel_height
        ),
        (1, 1)
    );
}

#[test]
fn iterm2_truncated_and_invalid_payloads_fail_closed() {
    let rgba = [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
    let mut png = encode_png_rgba(&rgba, 2, 2);
    png.truncate(png.len() / 2);

    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(&mut screen, &iterm2_inline(&png, "inline=1"));
    assert!(
        screen.images().is_empty(),
        "truncated PNG must not produce an image"
    );

    parser.parse(&mut screen, b"\x1b]1337;File=inline=1:!!!!\x07");
    assert!(
        screen.images().is_empty(),
        "invalid base64 must not produce an image"
    );
}

// ---------------------------------------------------------------------------
// Cross-protocol consistency vectors
// ---------------------------------------------------------------------------

/// 2x6 source with column 0 solid red and column 1 solid blue.
///
/// Sixel draws one 6-px column per `~` at P1=7 (1:1); Kitty/PNG use the same
/// row-major RGBA. All three must agree exactly for palette-exact primaries.
fn red_blue_2x6_rgba() -> Vec<u8> {
    let mut pixels = Vec::with_capacity(2 * 6 * 4);
    for _ in 0..6 {
        pixels.extend_from_slice(&[255, 0, 0, 255, 0, 0, 255, 255]);
    }
    pixels
}

#[test]
fn cross_protocol_red_blue_columns_agree_across_sixel_kitty_raw_and_png() {
    let expected = red_blue_2x6_rgba();

    // Sixel through the parser (shared palette path).
    let mut sixel_screen = make_screen();
    let mut parser = Parser::new();
    parser.parse(
        &mut sixel_screen,
        b"\x1bP7q#1;2;100;0;0~#2;2;0;0;100~\x1b\\",
    );
    assert_eq!(sixel_screen.images().len(), 1);
    assert_eq!(
        (
            sixel_screen.images()[0].pixel_width,
            sixel_screen.images()[0].pixel_height
        ),
        (2, 6)
    );
    assert_eq!(sixel_screen.images()[0].data.as_slice(), &expected);

    // Kitty raw RGBA through the parser.
    let mut raw_screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(
        &mut raw_screen,
        &kitty_apc("a=T,f=32,s=2,v=6,i=120,C=1", &expected),
    );
    assert_eq!(raw_screen.images()[0].data.as_slice(), &expected);

    // Kitty PNG carrying the same pixels.
    let png = encode_png_rgba(&expected, 2, 6);
    let mut png_screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(&mut png_screen, &kitty_apc("a=T,f=100,i=121,C=1", &png));
    assert_eq!(png_screen.images()[0].data.as_slice(), &expected);

    // iTerm2 PNG carrying the same pixels.
    let mut iterm_screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(&mut iterm_screen, &iterm2_inline(&png, "inline=1"));
    assert_eq!(iterm_screen.images()[0].data.as_slice(), &expected);
}

#[test]
fn cross_protocol_kitty_png_matches_kitty_raw_exactly() {
    // Four distinct corners exercise channel order (R/G/B/white).
    let rgba = [
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
    ];
    let png = encode_png_rgba(&rgba, 2, 2);

    let mut raw_screen = make_small_screen();
    Parser::new().parse(
        &mut raw_screen,
        &kitty_apc("a=T,f=32,s=2,v=2,i=122,C=1", &rgba),
    );
    let mut png_screen = make_small_screen();
    Parser::new().parse(&mut png_screen, &kitty_apc("a=T,f=100,i=123,C=1", &png));
    assert_eq!(
        raw_screen.images()[0].data.as_slice(),
        png_screen.images()[0].data.as_slice()
    );
}

#[test]
fn cross_protocol_jpeg_delta_is_explicit_and_bounded() {
    // Lossy JPEG cannot promise byte equality: record the accepted delta
    // explicitly. 8x8 solid-ish primaries at quality 95 stay close.
    let mut rgb = Vec::with_capacity(8 * 8 * 3);
    for y in 0..8 {
        for x in 0..8 {
            if x < 4 && y < 4 {
                rgb.extend_from_slice(&[255, 0, 0]);
            } else if x >= 4 && y < 4 {
                rgb.extend_from_slice(&[0, 255, 0]);
            } else if x < 4 {
                rgb.extend_from_slice(&[0, 0, 255]);
            } else {
                rgb.extend_from_slice(&[255, 255, 255]);
            }
        }
    }
    let jpeg = encode_jpeg_rgb(&rgb, 8, 8);
    assert!(jpeg.starts_with(&[0xFF, 0xD8, 0xFF]));
    let decoded = decode_image(&jpeg).expect("fixture JPEG must decode");
    assert_eq!((decoded.width, decoded.height), (8, 8));

    let mut rgba_expected = Vec::with_capacity(8 * 8 * 4);
    for px in rgb.as_chunks::<3>().0 {
        rgba_expected.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    let delta = max_channel_delta(&decoded.data, &rgba_expected);
    // Documented acceptance: quality-95 block primaries stay within a
    // small per-channel error. Tighten only with a corpus change.
    assert!(
        delta <= 32,
        "JPEG-vs-source max channel delta {delta} exceeds documented bound 32"
    );
}

// ---------------------------------------------------------------------------
// Placement, overlay order, and transient handling (observable pipeline)
// ---------------------------------------------------------------------------

#[test]
fn placement_preserves_text_and_exposes_layer_metadata() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(&mut screen, b"X");
    screen.cursor.col = 0;
    screen.cursor.row = 0;
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=1,v=1,i=130,z=-1,C=1", &[1, 2, 3, 4]),
    );
    assert_eq!(screen.get_cell(0, 0).unwrap().text(), "X");
    let image = &screen.images()[0];
    assert_eq!(image.z_index, -1);
    assert_eq!(image.layer(), ImageLayer::BehindText);
    assert_eq!(image.protocol_image_id, 130);
}

#[test]
fn placement_cursor_advance_and_suppression_are_deterministic() {
    let mut screen = Screen::new(6, 6, ScreenConfig::default());
    let mut parser = Parser::new();
    screen.cursor.col = 1;
    screen.cursor.row = 1;
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=1,v=1,c=2,r=3,i=131", &[1, 2, 3, 4]),
    );
    assert_eq!((screen.cursor.col, screen.cursor.row), (3, 3));

    let mut screen = Screen::new(6, 6, ScreenConfig::default());
    let mut parser = Parser::new();
    screen.cursor.col = 1;
    screen.cursor.row = 1;
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=1,v=1,c=2,r=3,i=132,C=1", &[1, 2, 3, 4]),
    );
    assert_eq!(
        (screen.cursor.col, screen.cursor.row),
        (1, 1),
        "C=1 must suppress cursor motion"
    );
}

/// Straight-alpha OVER blend of one RGBA pixel over another, truncating
/// integer math. Reference software composite for the
/// renderer-independent z-order contract: `BehindCellBackground` < cell
/// background < `BehindText` < text < `AboveText`. Cocoa, GTK, and
/// Direct2D implement this same order; this pins the blend here so native
/// pixel assertions have a headless reference (backends quantize
/// identically for these fixtures; premultiplied rounding may differ by
/// at most one step on other inputs).
fn alpha_over(dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
    let (dr, dg, db, da) = (
        u32::from(dst[0]),
        u32::from(dst[1]),
        u32::from(dst[2]),
        u32::from(dst[3]),
    );
    let (sr, sg, sb, sa) = (
        u32::from(src[0]),
        u32::from(src[1]),
        u32::from(src[2]),
        u32::from(src[3]),
    );
    let inv = 255 - sa;
    let out_a = sa + da * inv / 255;
    if out_a == 0 {
        return [0, 0, 0, 0];
    }
    let blend = |s: u32, d: u32| ((s * sa + d * da * inv / 255) / out_a) as u8;
    [blend(sr, dr), blend(sg, dg), blend(sb, db), out_a as u8]
}

#[test]
fn kitty_layered_placements_composite_in_documented_z_order() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    screen.cursor.col = 0;
    screen.cursor.row = 0;
    // Three opaque 1x1 placements stacked on the same cell, one per layer.
    // C=1 keeps the cursor home so every placement lands on (0, 0).
    for (id, z, rgba) in [
        (140, -2_000_000_000, [255, 0, 0, 255]),
        (141, -1, [0, 255, 0, 255]),
        (142, 5, [0, 0, 255, 255]),
    ] {
        let control = format!("a=T,f=32,s=1,v=1,i={id},z={z},C=1");
        parser.parse(&mut screen, &kitty_apc(&control, &rgba));
    }
    assert_eq!(screen.images().len(), 3);
    for image in screen.images() {
        assert_eq!((image.col, screen.image_visible_row(image)), (0, Some(0)));
        assert_eq!((image.pixel_width, image.pixel_height), (1, 1));
    }
    let by_protocol = |id: u32| {
        screen
            .images()
            .into_iter()
            .find(|image| image.protocol_image_id == id)
            .unwrap_or_else(|| panic!("placement {id} must be stored"))
    };
    assert_eq!(by_protocol(140).layer(), ImageLayer::BehindCellBackground);
    assert_eq!(by_protocol(141).layer(), ImageLayer::BehindText);
    assert_eq!(by_protocol(142).layer(), ImageLayer::AboveText);
    // Reference composite in paint order: red, then green, then blue.
    let mut pixel = [0, 0, 0, 0];
    for layer in [
        ImageLayer::BehindCellBackground,
        ImageLayer::BehindText,
        ImageLayer::AboveText,
    ] {
        for image in screen.visible_images_in_layer(layer) {
            let data = image.data.as_slice();
            pixel = alpha_over(pixel, [data[0], data[1], data[2], data[3]]);
        }
    }
    assert_eq!(
        pixel,
        [0, 0, 255, 255],
        "opaque blue on the top layer must win"
    );
}

#[test]
fn kitty_semitransparent_placement_blends_over_lower_layer() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    screen.cursor.col = 0;
    screen.cursor.row = 0;
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=1,v=1,i=143,z=-1,C=1", &[255, 0, 0, 255]),
    );
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=1,v=1,i=144,z=5,C=1", &[0, 0, 255, 128]),
    );
    assert_eq!(screen.images().len(), 2);
    let mut pixel = [0, 0, 0, 0];
    for layer in [ImageLayer::BehindText, ImageLayer::AboveText] {
        for image in screen.visible_images_in_layer(layer) {
            let data = image.data.as_slice();
            pixel = alpha_over(pixel, [data[0], data[1], data[2], data[3]]);
        }
    }
    // 50% blue over opaque red: exact under truncating integer math.
    assert_eq!(pixel, [127, 0, 128, 255]);
}

#[test]
fn animation_alpha_blend_preserves_destination_where_source_is_transparent() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=1,v=1,i=133,C=1", &[0, 0, 255, 255]),
    );
    // New frame based on frame 1 (c=1), AlphaBlend (X=0): transparent red
    // over opaque blue must leave blue.
    parser.parse(
        &mut screen,
        &kitty_apc("a=f,i=133,c=1,f=32,s=1,v=1,X=0", &[255, 0, 0, 0]),
    );
    parser.parse(&mut screen, &kitty_control_only("a=a,i=133,c=2"));
    assert_eq!(screen.images().len(), 1);
    assert_eq!(screen.images()[0].data.as_slice(), &[0, 0, 255, 255]);
}

#[test]
fn animation_overwrite_replaces_destination_even_with_transparent_source() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=1,v=1,i=134,C=1", &[0, 0, 255, 255]),
    );
    // Same setup with Overwrite (X=1): the transparent source wins.
    parser.parse(
        &mut screen,
        &kitty_apc("a=f,i=134,c=1,f=32,s=1,v=1,X=1", &[255, 0, 0, 0]),
    );
    parser.parse(&mut screen, &kitty_control_only("a=a,i=134,c=2"));
    assert_eq!(screen.images()[0].data.as_slice(), &[255, 0, 0, 0]);
}

#[test]
fn transient_hint_does_not_change_stored_pixels() {
    // Eviction priority itself is covered by the quota-aware store tests in
    // `kitty_graphics/tests.rs`; the pipeline must store transient uploads
    // with identical pixels so the hint never corrupts data.
    for (id, control) in [
        (135, "a=t,f=32,s=1,v=1,i=135"),
        (136, "a=t,f=32,s=1,v=1,i=136,N=1"),
    ] {
        let mut screen = make_small_screen();
        let mut parser = Parser::new();
        parser.parse(&mut screen, &kitty_apc(control, &[9, 8, 7, 255]));
        parser.parse(
            &mut screen,
            format!("\x1b_Ga=p,i={id},p=1,C=1\x1b\\").as_bytes(),
        );
        assert_eq!(screen.images()[0].data.as_slice(), &[9, 8, 7, 255]);
    }
}

// ---------------------------------------------------------------------------
// Hostile bounds: fail closed within documented budgets
// ---------------------------------------------------------------------------

#[test]
fn hostile_kitty_zero_and_oversized_dimensions_are_rejected() {
    for control in [
        "a=T,f=32,s=0,v=1,i=140",
        "a=T,f=32,s=1,v=0,i=141",
        "a=T,f=32,s=10001,v=1,i=142",
        "a=T,f=32,s=1,v=10001,i=143",
        "a=T,f=24,s=10001,v=10001,i=144",
    ] {
        let mut screen = make_small_screen();
        let mut parser = Parser::new();
        parser.parse(&mut screen, &kitty_apc(control, &[1, 2, 3, 4]));
        assert!(
            screen.images().is_empty(),
            "dimensions must be rejected: {control}"
        );
        let reply = String::from_utf8(screen.take_pending_responses().pop().unwrap()).unwrap();
        assert!(
            reply.contains("EINVAL") || reply.contains("ENOSPC"),
            "{control} must fail closed, got: {reply}"
        );
    }
}

#[test]
fn hostile_kitty_rgb_expansion_cannot_exceed_decoded_budget() {
    // 10_000 x 1_700 RGB needs 51 MiB encoded but 68 MiB as RGBA: the
    // expansion check must reject before allocating.
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=24,s=10000,v=1700,i=145,C=1", &[]),
    );
    assert!(screen.images().is_empty());
    let reply = String::from_utf8(screen.take_pending_responses().pop().unwrap()).unwrap();
    assert!(
        reply.contains("ENOSPC"),
        "RGB expansion bomb must report ENOSPC, got: {reply}"
    );
}

#[test]
fn hostile_kitty_png_beyond_image_decode_cap_is_rejected() {
    // `image_decode` caps at 4096 px; a 5000x1 PNG is cheap (20 KiB) but
    // must still fail closed through the Kitty PNG path.
    let wide = [200, 10, 20, 255].repeat(5000);
    let png = encode_png_rgba(&wide, 5000, 1);
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(&mut screen, &kitty_apc("a=T,f=100,i=146,C=1", &png));
    assert!(screen.images().is_empty());
    let reply = String::from_utf8(screen.take_pending_responses().pop().unwrap()).unwrap();
    assert!(
        reply.contains("EINVAL") || reply.contains("ENOSPC"),
        "oversized PNG must fail closed, got: {reply}"
    );
}

#[test]
fn hostile_image_decode_dimension_cap_is_enforced_cheaply() {
    // Narrow-but-wide images prove the 4096 px cap without huge buffers.
    let wide = [1, 2, 3, 255].repeat(5000);
    let png = encode_png_rgba(&wide, 5000, 1);
    let err = format!("{:?}", decode_image(&png).unwrap_err());
    assert!(
        err.contains("TooLarge"),
        "5000x1 must hit the dimension cap, got: {err}"
    );
}

#[test]
fn hostile_sixel_oversized_raster_truncates_within_budget() {
    let mut decoder = SixelDecoder::with_params(&[7]);
    put_all(&mut decoder, b"\"1;1;10001;1~");
    let result = decoder.finish_with_palette();
    assert!(
        result.truncated,
        "raster hint beyond 10_000 px must set truncated"
    );
    let image = result.image.unwrap();
    // Only the single sixel column survives; the 10001-wide hint is dropped.
    assert_eq!((image.width, image.height), (1, 6));
    assert!(image.data.len() <= DEFAULT_SIXEL_MAX_BYTES);
}

#[test]
fn hostile_sixel_repeat_bomb_truncates_at_max_width() {
    let mut decoder = SixelDecoder::with_params(&[7]);
    put_all(&mut decoder, b"!20000~");
    let result = decoder.finish_with_palette();
    assert!(result.truncated);
    let image = result.image.unwrap();
    assert_eq!(image.width, MAX_SIXEL_DIMENSION);
    assert!(image.data.len() <= DEFAULT_SIXEL_MAX_BYTES);
}

#[test]
fn hostile_sixel_palette_management_clamps_to_foot_range() {
    let mut screen = make_screen();
    let mut parser = Parser::new();
    // Request 9999 colors via Sixel resource management: clamped to 1024
    // with a foot-compatible reply, never a 9999-entry allocation spike.
    parser.parse(&mut screen, b"\x1b[?1;3;9999S");
    assert_eq!(
        screen.take_pending_responses(),
        vec![b"\x1b[?1;0;1024S".to_vec()]
    );
    // Oversized DCS repeat still decodes a bounded prefix through the parser.
    parser.parse(&mut screen, b"\x1bP7q!20000~\x1b\\");
    let images = screen.images();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].pixel_width, MAX_SIXEL_DIMENSION);
}

#[test]
fn hostile_iterm2_oversized_image_fails_closed() {
    let wide = [9, 9, 9, 255].repeat(5000);
    let png = encode_png_rgba(&wide, 5000, 1);
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(&mut screen, &iterm2_inline(&png, "inline=1"));
    assert!(
        screen.images().is_empty(),
        "5000 px iTerm2 image must not display (4096 cap)"
    );
}

#[test]
fn hostile_kitty_bad_chunk_marker_is_rejected() {
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=1,v=1,i=147,m=2", &[1, 2, 3, 4]),
    );
    assert!(screen.images().is_empty());
    let reply = String::from_utf8(screen.take_pending_responses().pop().unwrap()).unwrap();
    assert!(reply.contains("EINVAL"), "m=2 must be EINVAL, got: {reply}");
}

#[test]
fn hostile_truncated_kitty_png_fails_closed() {
    let rgba = [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
    let mut png = encode_png_rgba(&rgba, 2, 2);
    png.truncate(png.len() / 2);
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(&mut screen, &kitty_apc("a=T,f=100,i=148,C=1", &png));
    assert!(screen.images().is_empty());
    let reply = String::from_utf8(screen.take_pending_responses().pop().unwrap()).unwrap();
    assert!(
        reply.contains("EINVAL"),
        "truncated PNG must be EINVAL, got: {reply}"
    );
}

#[test]
fn hostile_cve_shaped_graphics_input_fails_closed_without_panic() {
    // Kitty CVE-2026-33633 shape: arbitrary garbage declared as a direct
    // PNG upload. Must fail closed, never panic, never store.
    let mut screen = make_small_screen();
    let mut parser = Parser::new();
    parser.parse(
        &mut screen,
        &kitty_apc(
            "a=T,f=100,i=200,C=1",
            b"definitely not a png \x00\xff\x89PNG\r\n trailing garbage",
        ),
    );
    assert!(screen.images().is_empty());
    assert_eq!(screen.take_pending_responses().len(), 1);

    // PNG magic with lying dimensions but no backing data: the dimension
    // cap must reject cheaply, without multi-gigabyte allocation attempts.
    let mut lying = b"\x89PNG\r\n\x1a\n".to_vec();
    lying.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
    lying.extend_from_slice(&[0x0f, 0x27, 0x00, 0x0f, 0x27, 0x00]); // 999999x999999
    lying.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
    parser.parse(&mut screen, &kitty_apc("a=T,f=100,i=201,C=1", &lying));
    assert!(screen.images().is_empty());

    // Kitty CVE-2026-33642 shape: compose commands with hostile offsets,
    // against a live image and against nothing at all.
    parser.parse(
        &mut screen,
        &kitty_apc("a=T,f=32,s=1,v=1,i=202,C=1", &[9, 9, 9, 255]),
    );
    parser.parse(
        &mut screen,
        &kitty_apc("a=f,i=202,c=1,f=32,s=1,v=1,X=0", &[1, 2, 3, 255]),
    );
    for control in [
        "a=c,i=202,r=2,c=1,X=1,Y=0,x=-5,y=-5,w=1,h=1,C=1",
        "a=c,i=202,r=2,c=1,X=1,Y=0,x=4294967295,y=4294967295,w=1,h=1,C=1",
        "a=c,i=202,r=2,c=1,X=2147483647,Y=2147483647,x=0,y=0,w=2147483647,h=2147483647,C=1",
        "a=c,i=999,r=2,c=1,X=1,Y=0,x=0,y=0,w=1,h=1,C=1",
    ] {
        parser.parse(&mut screen, &kitty_control_only(control));
    }
    // Every hostile compose drew an error reply; the base image still has
    // consistent dimensions and data.
    let replies = screen.take_pending_responses();
    assert!(
        replies.len() >= 4,
        "expected one error reply per hostile compose, got {}",
        replies.len()
    );
    for image in screen.images() {
        assert_eq!(image.data.len(), image.pixel_width * image.pixel_height * 4);
    }
}
