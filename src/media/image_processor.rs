//! `PureRustImageProcessor` (design.md "Processing / 画像処理層（ネイティブ
//! 依存ゲート）" -> "MediaProcessor（port）/ PureRustImageProcessor（adapter）",
//! Requirements 6.1, 6.2, 6.3, 6.4, 6.5, 10.1, 10.2, 10.3, 10.4; task 2.3,
//! `Boundary: MediaProcessor, PureRustImageProcessor`).
//!
//! Scope: this module owns exactly the pure-Rust [`MediaProcessor`] adapter
//! — [`PureRustImageProcessor`] — the one place in this crate allowed to
//! depend on an actual image-decoding/encoding crate (the `image` crate)
//! and a BlurHash crate (the `blurhash` crate). Both are pure Rust with no
//! `*-sys`/native/C dependency in their own dependency trees (verified via
//! `cargo tree`; see this task's status report CONCERNS for the exact
//! versions and the pure-Rust decision this reflects, research.md's
//! "ネイティブ依存判断ゲート" and design.md's Technology Stack "Media
//! Processing" row) — satisfying Requirement 10.2's "MVP の画像処理が要求
//! するネイティブ依存の範囲...pure-Rust で賄える範囲" decision.
//!
//! ## What this adapter does (Requirements 6.1, 6.2, 6.3)
//! `process_image` decodes `original` (format auto-detected from its own
//! byte signature by `image::load_from_memory` — the same four raster
//! formats `crate::config::MediaConfig::supported_formats` defaults to,
//! `image/jpeg`/`image/png`/`image/gif`/`image/webp`, are exactly the
//! `image` crate features this crate's `Cargo.toml` enables: `jpeg`,
//! `png`, `gif`, `webp`, with `default-features = false` so none of the
//! crate's other formats — several of which, e.g. `avif`, pull in a much
//! heavier pure-Rust encoder stack — are compiled in unnecessarily), fits a
//! thumbnail within `thumb_target`'s bounding box without ever upscaling
//! past the original's own size (see [`fit_within`]), resizes with a
//! `Lanczos3` filter, computes a BlurHash from the resized RGBA pixels
//! (`4x3` components — the same component counts Mastodon's own reference
//! implementation defaults to), and encodes the thumbnail as PNG (chosen
//! over JPEG/GIF/WebP as the one lossless, alpha-capable format this
//! adapter always has an encoder for regardless of the *input* format,
//! keeping thumbnail encoding format-independent and deterministic; see
//! [`THUMBNAIL_CONTENT_TYPE`]).
//!
//! ## Determinism (Requirement 6.4)
//! Every step — decode, resize, BlurHash, PNG encode — is a pure function
//! of the input bytes plus `thumb_target`: no wall-clock read, no RNG, no
//! filesystem/network I/O, no HashMap-iteration-order-dependent output.
//! This module's `determinism` tests call `process_image` twice on
//! identical input and assert the two `ProcessedImage`s compare `==` in
//! full (thumbnail bytes included, not just dimensions/BlurHash), which
//! would fail if this adapter ever introduced e.g. a real random seed, a
//! timestamp embedded in the PNG, or a HashMap-ordered encoding step.
//!
//! ## Failure (Requirement 6.5)
//! `image::load_from_memory` returns `Err` (not a panic) for corrupt or
//! unrecognized-format bytes; this adapter maps that to
//! `AppError::client(UNPROCESSABLE_ENTITY, ...)` (a bad *input*, matching
//! `store.rs`'s existing precedent of using `AppError::client` for a
//! caller-facing condition even in a component with no direct HTTP surface
//! of its own) rather than unwrapping or panicking. This module's
//! `corrupt_input`/`empty_input`/`truncated_input` tests exercise that path
//! directly.
//!
//! ## Out of scope
//! Video/audio processing (Requirement 10.3 — no such code exists here,
//! deliberately, per research.md's "動画は後回し" decision).
//! `MediaRepository`/`ProcessingJobQueue` (tasks 3.1/3.2), `MediaService`
//! (task 4.1), and `ProcessingWorker` (task 4.3, the actual future caller
//! of this adapter through the `MediaProcessor` trait) do not exist yet and
//! are not referenced here.

use std::io::Cursor;

use axum::http::StatusCode;
use image::{GenericImageView, ImageFormat, imageops::FilterType};

use crate::error::AppError;
use crate::media::model::Dimensions;
use crate::media::processor::{MediaProcessor, ProcessedImage, ThumbnailSpec};

/// BlurHash horizontal component count — matches the Mastodon reference
/// implementation's own default (design.md/research.md do not pin an exact
/// number; `4x3` is the widely-used convention this crate's downstream
/// `MediaAttachmentSerializer`, task 4.2, will hand to Mastodon-compatible
/// clients that assume it).
const BLURHASH_COMPONENTS_X: u32 = 4;
/// BlurHash vertical component count, paired with
/// [`BLURHASH_COMPONENTS_X`].
const BLURHASH_COMPONENTS_Y: u32 = 3;

/// The content type a generated thumbnail is always encoded as, regardless
/// of the original's own format (see this module's doc comment for why
/// PNG).
const THUMBNAIL_CONTENT_TYPE: &str = "image/png";

/// pure-Rust [`MediaProcessor`] adapter (design.md's
/// "PureRustImageProcessor（adapter）", Requirements 6.1-6.5, 10.1-10.4).
///
/// Stateless and zero-sized: every input `process_image` needs is passed as
/// an argument, so there is nothing to configure or hold onto at
/// construction time (unlike `LocalFsStore`, which holds a `root` path).
#[derive(Debug, Clone, Copy, Default)]
pub struct PureRustImageProcessor;

impl PureRustImageProcessor {
    /// Builds a `PureRustImageProcessor`. Trivial (the type carries no
    /// state) but provided for symmetry with `LocalFsStore::new` and so
    /// call sites read `PureRustImageProcessor::new()` rather than relying
    /// on `Default`/unit-struct-literal construction.
    pub fn new() -> Self {
        PureRustImageProcessor
    }
}

impl MediaProcessor for PureRustImageProcessor {
    fn process_image(
        &self,
        original: &[u8],
        thumb_target: ThumbnailSpec,
    ) -> Result<ProcessedImage, AppError> {
        let decoded = image::load_from_memory(original).map_err(|source| {
            AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("failed to decode image: {source}"),
            )
        })?;

        let (orig_width, orig_height) = decoded.dimensions();
        if orig_width == 0 || orig_height == 0 {
            return Err(AppError::client(
                StatusCode::UNPROCESSABLE_ENTITY,
                "decoded image has zero width or height",
            ));
        }
        let original_dims = dimensions_of(orig_width, orig_height);

        let (thumb_width, thumb_height) = fit_within(
            orig_width,
            orig_height,
            thumb_target.target_width.max(1),
            thumb_target.target_height.max(1),
        );
        let resized = decoded.resize_exact(thumb_width, thumb_height, FilterType::Lanczos3);
        let thumbnail_dims = dimensions_of(thumb_width, thumb_height);

        let rgba = resized.to_rgba8();
        let blurhash = blurhash::encode(
            BLURHASH_COMPONENTS_X,
            BLURHASH_COMPONENTS_Y,
            thumb_width,
            thumb_height,
            rgba.as_raw(),
        )
        .map_err(|source| {
            AppError::server(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("blurhash generation failed: {source}"),
            )
        })?;

        let mut thumbnail = Vec::new();
        resized
            .write_to(&mut Cursor::new(&mut thumbnail), ImageFormat::Png)
            .map_err(|source| AppError::server(StatusCode::INTERNAL_SERVER_ERROR, source))?;

        Ok(ProcessedImage {
            thumbnail,
            thumbnail_dims,
            original_dims,
            blurhash,
            content_type: THUMBNAIL_CONTENT_TYPE.to_string(),
        })
    }
}

/// Builds a [`Dimensions`] from a decoded/resized image's pixel size,
/// precomputing its aspect ratio (Requirement 6.3).
fn dimensions_of(width: u32, height: u32) -> Dimensions {
    Dimensions {
        width,
        height,
        aspect: width as f32 / height as f32,
    }
}

/// Computes the largest `(width, height)` that (a) fits within the
/// `max_width` x `max_height` bounding box, (b) preserves `orig_width` /
/// `orig_height`'s aspect ratio, and (c) never exceeds the original's own
/// dimensions (no upscaling) — unlike `DynamicImage::resize`/`thumbnail`,
/// which both scale *up* to fill the requested box when the source is
/// smaller than it (see this module's doc comment / this task's own
/// research into `image` crate's `resize_dimensions` helper). Both
/// `orig_width`/`orig_height` and `max_width`/`max_height` are assumed
/// non-zero by the caller (`process_image` checks the former, and clamps
/// the latter with `.max(1)` before calling this).
fn fit_within(orig_width: u32, orig_height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let width_ratio = f64::from(max_width) / f64::from(orig_width);
    let height_ratio = f64::from(max_height) / f64::from(orig_height);
    let ratio = width_ratio.min(height_ratio).min(1.0);
    let new_width = ((f64::from(orig_width) * ratio).round() as u32).max(1);
    let new_height = ((f64::from(orig_height) * ratio).round() as u32).max(1);
    (new_width, new_height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Rgba, RgbaImage};

    /// Builds a small deterministic-content RGBA test image (not a solid
    /// color — a gradient — so resizing/BlurHash actually has non-trivial
    /// pixel data to work with, rather than every algorithm degenerating to
    /// a single-color trivial case).
    fn sample_rgba(width: u32, height: u32) -> RgbaImage {
        ImageBuffer::from_fn(width, height, |x, y| {
            let r = (x * 255 / width.max(1)) as u8;
            let g = (y * 255 / height.max(1)) as u8;
            let b = 128u8;
            Rgba([r, g, b, 255])
        })
    }

    fn encode(image: &DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), format)
            .expect("encoding the in-memory fixture image must succeed");
        bytes
    }

    fn sample_png(width: u32, height: u32) -> Vec<u8> {
        encode(
            &DynamicImage::ImageRgba8(sample_rgba(width, height)),
            ImageFormat::Png,
        )
    }

    fn sample_jpeg(width: u32, height: u32) -> Vec<u8> {
        // JPEG has no alpha channel; drop to RGB8 before encoding so the
        // encoder does not have to synthesize one.
        let rgb = DynamicImage::ImageRgba8(sample_rgba(width, height)).to_rgb8();
        encode(&DynamicImage::ImageRgb8(rgb), ImageFormat::Jpeg)
    }

    fn sample_gif(width: u32, height: u32) -> Vec<u8> {
        encode(
            &DynamicImage::ImageRgba8(sample_rgba(width, height)),
            ImageFormat::Gif,
        )
    }

    fn sample_webp(width: u32, height: u32) -> Vec<u8> {
        encode(
            &DynamicImage::ImageRgba8(sample_rgba(width, height)),
            ImageFormat::WebP,
        )
    }

    #[test]
    fn process_image_decodes_a_valid_png_and_reports_original_dimensions() {
        let processor = PureRustImageProcessor::new();
        let png = sample_png(64, 32);
        let result = processor
            .process_image(&png, ThumbnailSpec::new(400, 400))
            .expect("a valid PNG must process successfully");
        assert_eq!(result.original_dims.width, 64);
        assert_eq!(result.original_dims.height, 32);
        assert!((result.original_dims.aspect - 2.0).abs() < 1e-4);
    }

    #[test]
    fn process_image_decodes_all_four_configured_supported_formats() {
        // Mirrors `crate::config::MediaConfig::supported_formats`'s default
        // four content types (`image/jpeg`, `image/png`, `image/gif`,
        // `image/webp`) — every one of them must actually decode through
        // this adapter, not just PNG.
        let processor = PureRustImageProcessor::new();
        for (label, bytes) in [
            ("png", sample_png(40, 20)),
            ("jpeg", sample_jpeg(40, 20)),
            ("gif", sample_gif(40, 20)),
            ("webp", sample_webp(40, 20)),
        ] {
            let result = processor.process_image(&bytes, ThumbnailSpec::new(400, 400));
            assert!(
                result.is_ok(),
                "expected {label} to decode successfully, got {:?}",
                result.err()
            );
            let result = result.unwrap();
            assert_eq!(result.original_dims.width, 40, "format {label}");
            assert_eq!(result.original_dims.height, 20, "format {label}");
        }
    }

    #[test]
    fn process_image_produces_a_thumbnail_downscaled_to_fit_within_the_target_box() {
        let processor = PureRustImageProcessor::new();
        let png = sample_png(800, 400);
        let result = processor
            .process_image(&png, ThumbnailSpec::new(100, 100))
            .unwrap();
        assert!(result.thumbnail_dims.width <= 100);
        assert!(result.thumbnail_dims.height <= 100);
        // Aspect ratio (2:1) is preserved in the downscaled thumbnail.
        assert_eq!(result.thumbnail_dims.width, 100);
        assert_eq!(result.thumbnail_dims.height, 50);
        // The thumbnail is genuinely smaller than the original, and the
        // returned bytes actually decode back to those dimensions (proving
        // `thumbnail` is real encoded image data, not a stub/placeholder).
        let decoded_thumb = image::load_from_memory(&result.thumbnail)
            .expect("the returned thumbnail bytes must themselves be a valid image");
        assert_eq!(decoded_thumb.dimensions(), (100, 50));
    }

    #[test]
    fn process_image_does_not_upscale_an_image_smaller_than_the_target_box() {
        let processor = PureRustImageProcessor::new();
        let png = sample_png(20, 10);
        let result = processor
            .process_image(&png, ThumbnailSpec::new(400, 400))
            .unwrap();
        // The source is already smaller than the target box on both axes;
        // the thumbnail must stay at the original size, not be scaled up.
        assert_eq!(result.thumbnail_dims.width, 20);
        assert_eq!(result.thumbnail_dims.height, 10);
    }

    #[test]
    fn process_image_produces_a_non_empty_blurhash_string() {
        let processor = PureRustImageProcessor::new();
        let png = sample_png(32, 32);
        let result = processor
            .process_image(&png, ThumbnailSpec::new(200, 200))
            .unwrap();
        assert!(!result.blurhash.is_empty());
        // A real BlurHash is short ASCII, not e.g. a placeholder echoing
        // input length or a UUID.
        assert!(result.blurhash.len() < 64);
        assert!(result.blurhash.is_ascii());
    }

    #[test]
    fn process_image_blurhash_differs_for_visibly_different_images() {
        let processor = PureRustImageProcessor::new();
        let gradient = sample_png(32, 32);
        let solid = encode(
            &DynamicImage::ImageRgba8(ImageBuffer::from_pixel(32, 32, Rgba([10, 200, 30, 255]))),
            ImageFormat::Png,
        );
        let hash_a = processor
            .process_image(&gradient, ThumbnailSpec::new(200, 200))
            .unwrap()
            .blurhash;
        let hash_b = processor
            .process_image(&solid, ThumbnailSpec::new(200, 200))
            .unwrap()
            .blurhash;
        assert_ne!(
            hash_a, hash_b,
            "a gradient and a solid-color image must not hash identically"
        );
    }

    #[test]
    fn process_image_reports_the_thumbnail_content_type() {
        let processor = PureRustImageProcessor::new();
        let jpeg = sample_jpeg(50, 50);
        let result = processor
            .process_image(&jpeg, ThumbnailSpec::new(200, 200))
            .unwrap();
        assert_eq!(result.content_type, "image/png");
    }

    #[test]
    fn process_image_is_deterministic_across_repeated_calls_on_identical_input() {
        // Requirement 6.4: same input + same ThumbnailSpec -> byte-identical
        // ProcessedImage (thumbnail bytes, BlurHash, dimensions) every time.
        let processor = PureRustImageProcessor::new();
        let png = sample_png(123, 77);
        let spec = ThumbnailSpec::new(64, 64);
        let first = processor.process_image(&png, spec).unwrap();
        let second = processor.process_image(&png, spec).unwrap();
        let third = processor.process_image(&png, spec).unwrap();
        assert_eq!(first, second);
        assert_eq!(second, third);
    }

    #[test]
    fn process_image_is_deterministic_across_independent_processor_instances() {
        // Determinism must not accidentally depend on some hidden per-instance
        // state (e.g. a processor built once vs. built fresh each call).
        let png = sample_png(90, 90);
        let spec = ThumbnailSpec::new(30, 30);
        let a = PureRustImageProcessor::new()
            .process_image(&png, spec)
            .unwrap();
        let b = PureRustImageProcessor::new()
            .process_image(&png, spec)
            .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn process_image_rejects_empty_input_with_an_error_not_a_panic() {
        let processor = PureRustImageProcessor::new();
        let result = processor.process_image(&[], ThumbnailSpec::new(100, 100));
        assert!(result.is_err());
    }

    #[test]
    fn process_image_rejects_garbage_bytes_with_an_error_not_a_panic() {
        let processor = PureRustImageProcessor::new();
        let garbage = vec![0xDEu8, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let result = processor.process_image(&garbage, ThumbnailSpec::new(100, 100));
        assert!(result.is_err());
    }

    #[test]
    fn process_image_rejects_a_truncated_png_with_an_error_not_a_panic() {
        let processor = PureRustImageProcessor::new();
        let full_png = sample_png(64, 64);
        // Chop off the back half of an otherwise-valid PNG: the signature
        // and header parse, but decoding the image data must fail cleanly.
        let truncated = &full_png[..full_png.len() / 2];
        let result = processor.process_image(truncated, ThumbnailSpec::new(100, 100));
        assert!(
            result.is_err(),
            "a truncated PNG must be rejected, not silently accepted"
        );
    }

    #[test]
    fn process_image_error_is_a_client_error_not_a_server_panic_path() {
        let processor = PureRustImageProcessor::new();
        let err = processor
            .process_image(b"not an image", ThumbnailSpec::new(100, 100))
            .expect_err("garbage bytes must be rejected");
        assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn fit_within_preserves_aspect_ratio_when_downscaling() {
        assert_eq!(fit_within(1000, 500, 100, 100), (100, 50));
        assert_eq!(fit_within(500, 1000, 100, 100), (50, 100));
    }

    #[test]
    fn fit_within_never_upscales() {
        assert_eq!(fit_within(10, 10, 400, 400), (10, 10));
        assert_eq!(fit_within(10, 5, 400, 400), (10, 5));
    }

    #[test]
    fn fit_within_returns_at_least_one_pixel_on_each_axis() {
        let (w, h) = fit_within(1000, 1, 1, 1000);
        assert!(w >= 1);
        assert!(h >= 1);
    }

    #[test]
    fn pure_rust_image_processor_is_usable_through_the_media_processor_trait_alone() {
        // Demonstrates a caller depending only on `MediaProcessor` (not the
        // concrete `PureRustImageProcessor` type) can still drive this
        // adapter end to end (Requirement 10.1, 10.4).
        fn process_via_trait(processor: &dyn MediaProcessor, bytes: &[u8]) -> ProcessedImage {
            processor
                .process_image(bytes, ThumbnailSpec::new(50, 50))
                .unwrap()
        }
        let processor = PureRustImageProcessor::new();
        let png = sample_png(60, 60);
        let result = process_via_trait(&processor, &png);
        assert_eq!(result.original_dims.width, 60);
    }
}
