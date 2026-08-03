//! `MediaProcessor` port and its result/parameter types (design.md
//! "Processing / 画像処理層（ネイティブ依存ゲート）" -> "MediaProcessor
//! （port）/ PureRustImageProcessor（adapter）", Requirements 6.1, 6.2, 6.3,
//! 6.4, 6.5, 10.1, 10.2, 10.3, 10.4; task 2.3, `Boundary: MediaProcessor,
//! PureRustImageProcessor`).
//!
//! Scope: this module owns exactly the image-processing abstraction
//! boundary itself — the [`MediaProcessor`] trait plus the two plain value
//! types a call to it exchanges, [`ThumbnailSpec`] (input parameter) and
//! [`ProcessedImage`] (output). It has no concrete decode/resize/encode/
//! BlurHash logic at all — that is [`crate::media::image_processor`]'s
//! [`PureRustImageProcessor`] adapter, the only thing in this crate allowed
//! to depend on an actual image-processing crate (Requirement 10.1: "画像
//! の復号・縮小・符号化・BlurHash 生成を処理抽象の背後に隔離し、ネイティブ
//! 依存の有無を呼び出し側へ波及させない"). A caller (a later
//! `ProcessingWorker`, task 4.3) depends on nothing but this trait, so a
//! future native-dependency adapter (e.g. libvips-backed) can replace
//! `PureRustImageProcessor` without this module, or any caller, changing
//! (Requirement 10.4).
//!
//! No persistence (`MediaRepository`, task 3.1), storage (`MediaStore`,
//! task 2.2, already implemented — this module does not touch it), business
//! logic (`MediaService`, task 4.1), or HTTP surface (`MediaEndpoints`,
//! task 5.1) lives here.
//!
//! ## `ThumbnailSpec`: inferred shape, not literally sketched by design.md
//! design.md's Service Interface excerpt for `MediaProcessor` writes
//! `thumb_target: ThumbnailSpec` without spelling out `ThumbnailSpec`'s own
//! fields anywhere in the document. The one concrete clue is
//! `crate::config::MediaConfig::thumbnail_target_width`/
//! `thumbnail_target_height` (`src/config.rs`, task 1.2) — the only startup
//! setting this feature has for "what size should a thumbnail be" — so
//! `ThumbnailSpec` is modeled as exactly that pair, `target_width`/
//! `target_height` in pixels, describing the *maximum bounding box* a
//! generated thumbnail must fit within (see `image_processor.rs`'s
//! `fit_within` for the aspect-preserving, non-upscaling fit computation
//! that consumes it). This mirrors task 2.2's already-accepted precedent
//! (`store.rs`'s `public_url` deviating from the design-doc-sketched
//! `RequestUriContext` parameter type) for inferring/adapting an
//! underspecified design.md signature detail from surrounding context
//! rather than blocking on it.

use crate::error::AppError;
use crate::media::model::Dimensions;

/// The maximum width/height (in pixels) a generated thumbnail must fit
/// within, preserving the original image's aspect ratio and never
/// upscaling past the original's own dimensions (Requirement 6.1). A
/// caller constructs this from
/// `crate::config::MediaConfig::thumbnail_target_width`/
/// `thumbnail_target_height` — see this module's doc comment for why this
/// shape, not a literal design.md sketch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailSpec {
    pub target_width: u32,
    pub target_height: u32,
}

impl ThumbnailSpec {
    /// Builds a `ThumbnailSpec` targeting `target_width` x `target_height`
    /// pixels as the maximum bounding box for a generated thumbnail.
    pub fn new(target_width: u32, target_height: u32) -> Self {
        ThumbnailSpec {
            target_width,
            target_height,
        }
    }
}

/// The derivatives a successful [`MediaProcessor::process_image`] call
/// produces (design.md's `ProcessedImage` sketch, Requirements 6.1, 6.2,
/// 6.3): a generated thumbnail (already encoded, ready to hand to
/// `MediaStore::put`) plus its dimensions, the original (undecoded input's)
/// dimensions/aspect, a BlurHash placeholder string, and the thumbnail's
/// own encoded content type (the *original*'s content type is already
/// known to the caller from the upload's declared MIME type before
/// processing even starts — Requirement 1.3's validation happens earlier,
/// out of this port's boundary — so this field is unambiguously about the
/// newly-encoded thumbnail bytes, not the input).
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessedImage {
    pub thumbnail: Vec<u8>,
    pub thumbnail_dims: Dimensions,
    pub original_dims: Dimensions,
    pub blurhash: String,
    pub content_type: String,
}

/// Image-processing abstraction boundary (the native-dependency gate:
/// design.md "Processing / 画像処理層（ネイティブ依存ゲート）", Requirements
/// 6.1-6.5, 10.1-10.4).
///
/// A single synchronous method: decode `original`, generate a thumbnail
/// fitting within `thumb_target`, compute its BlurHash, and report both the
/// thumbnail's and the original's dimensions — or fail explicitly
/// (Requirement 6.5) rather than panic. Synchronous (not `async fn`) because
/// this is CPU-bound work with no I/O of its own (the input bytes and
/// output bytes are both already in memory; a caller like `ProcessingWorker`
/// owns any I/O, e.g. reading the original from `MediaStore` beforehand and
/// writing the thumbnail back afterward) — unlike `MediaStore` (task 2.2),
/// there is no analogous precedent here for native `async fn in trait`
/// usage because there is nothing to `.await`.
pub trait MediaProcessor: Send + Sync {
    /// Decodes `original`, generates a thumbnail fitting within
    /// `thumb_target`, and computes its BlurHash and dimension metadata.
    ///
    /// Postcondition (Requirement 6.4): calling this twice with the same
    /// `original` bytes and the same `thumb_target` must return
    /// `ProcessedImage`s that compare equal (same thumbnail bytes, same
    /// BlurHash string, same dimensions) — no wall-clock, randomness, or
    /// other non-deterministic input may influence the result.
    ///
    /// Errors (Requirement 6.5): a decode failure (corrupt or unsupported
    /// input bytes) or a derivative-generation failure returns
    /// `Err(AppError)` — it never panics.
    fn process_image(
        &self,
        original: &[u8],
        thumb_target: ThumbnailSpec,
    ) -> Result<ProcessedImage, AppError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_spec_new_stores_the_given_dimensions() {
        let spec = ThumbnailSpec::new(400, 300);
        assert_eq!(spec.target_width, 400);
        assert_eq!(spec.target_height, 300);
    }

    #[test]
    fn thumbnail_spec_is_copy_and_compares_by_value() {
        let a = ThumbnailSpec::new(400, 400);
        let b = a;
        assert_eq!(a, b);
        assert_ne!(a, ThumbnailSpec::new(400, 401));
    }
}
