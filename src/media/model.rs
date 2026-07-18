//! Media domain types (`model` component, design.md "Media Domain /
//! ドメイン層" -> `model`, Requirements 1.2, 4.1, 6.3, 7.1, 7.2; task 2.1).
//!
//! Scope: this module owns exactly the domain value types design.md's
//! `model` component sketches — [`Media`], [`MediaType`], [`MediaState`],
//! [`Focus`], [`Dimensions`], [`MediaMeta`], [`ProcessingJob`], and
//! [`JobState`] — plus the one invariant a type alone cannot express by
//! construction: a [`Focus`] coordinate pair must stay within the
//! Mastodon-compatible focal-point convention range `-1.0..=1.0` on both
//! axes (Requirements 7.1, 7.2). No persistence (`MediaRepository`,
//! `ProcessingJobQueue`, tasks 3.1/3.2), no storage (`MediaStore`, task
//! 2.2), no image processing (`MediaProcessor`, task 2.3), no business
//! logic (`MediaService`, task 4.1), and no HTTP surface (`MediaEndpoints`,
//! task 5.1) live here — those consume the types defined in this module but
//! are out of scope for task 2.1 (`Boundary: model`).
//!
//! [`Media::actor_id`] is a plain, non-`Option<Id>` field (Requirement
//! 1.2's "所有アクターを必須とする" — every media attachment is bound to
//! exactly one owning actor): the type system alone makes an ownerless
//! `Media` unconstructable, so no separate runtime check is needed for that
//! invariant, and this module's tests instead demonstrate it structurally
//! (exhaustive field destructuring, no `..`).
//!
//! [`Focus`] cannot make the same "unconstructable if invalid" argument
//! from field types alone (`f32` admits any value in range, including
//! `NaN`/out-of-range magnitudes), so [`Focus`] keeps its `x`/`y` fields
//! private and only [`Focus::new`] (fallible, returning
//! [`FocusRangeError`] on an out-of-range axis) or [`Focus::default`]
//! (fixed center `(0.0, 0.0)`, Requirement 7.2) can produce one — an
//! invalid `Focus` is rejected at construction time, never silently
//! clamped or accepted (Requirement 7.4 extends this same rejection to the
//! HTTP update path; this module only owns the value-type half of that
//! contract). `Media::state == MediaState::Ready` implying `Media::meta`
//! and a resolved media-entity URL are both present (design.md's model
//! Contracts note) is a state-machine invariant later components
//! (`MediaRepository`/`MediaService`) are responsible for upholding when
//! they transition a `Media`'s state — it is not enforced here because the
//! entity-URL half of that invariant is not even a field on this type (it
//! is resolved from `MediaStore`, task 2.2).

use time::OffsetDateTime;

use crate::domain::Id;

/// The kind of media a [`Media`] attachment holds.
///
/// The MVP only generates real derivatives (thumbnail/BlurHash/dimensions)
/// for [`MediaType::Image`] (design.md's type sketch comment, Requirement
/// 10.3); the other variants are represented so upload/serialization code
/// can classify an attachment even though only `Image` is processed end to
/// end in the MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaType {
    Image,
    Gifv,
    Video,
    Audio,
    Unknown,
}

/// A [`Media`] attachment's processing lifecycle state (Requirements 2.1,
/// 4.3, 4.5).
///
/// `Processing` is the state a newly accepted upload starts in (Requirement
/// 1.1); a successful processing job transitions it to `Ready` (Requirement
/// 4.3); exhausting the retry budget transitions it to `Failed`
/// (Requirement 4.5). There is no `Deleted`/`Pending` variant — those are
/// out of this spec's scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaState {
    Processing,
    Ready,
    Failed,
}

/// A [`media_processing_jobs`](../../../migrations/0005_media.sql) row's
/// own lifecycle state (Requirement 4.1-4.6).
///
/// There is no `Done`/`Completed` variant: a successfully completed job is
/// finalized by deleting (or otherwise retiring) its queue row rather than
/// holding it in a terminal "done" state — `ProcessingJobQueue::complete`
/// (task 3.2) owns that transition, matching
/// `migrations/0005_media.sql`'s `state` column comment
/// (`queued`/`processing`/`failed` only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobState {
    Queued,
    Processing,
    Failed,
}

/// Lower bound (inclusive) of a valid [`Focus`] coordinate, per the
/// Mastodon-compatible focal-point convention (Requirements 7.1, 7.2).
pub const FOCUS_MIN: f32 = -1.0;

/// Upper bound (inclusive) of a valid [`Focus`] coordinate.
pub const FOCUS_MAX: f32 = 1.0;

/// A validated focal-point coordinate pair, constrained to
/// `-1.0..=1.0` on both axes (Requirement 7.1), defaulting to the center
/// `(0.0, 0.0)` when unspecified (Requirement 7.2).
///
/// `x`/`y` are private specifically so that [`Focus::new`] is the only way
/// to construct a non-default `Focus` — see this module's doc comment for
/// why that is necessary (an out-of-range `f32` is not otherwise
/// unrepresentable).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Focus {
    x: f32,
    y: f32,
}

/// A [`Focus`] coordinate fell outside the accepted `-1.0..=1.0` range
/// (Requirement 7.4: an out-of-range focal point must be rejected, not
/// clamped or silently accepted).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusRangeError {
    pub axis: FocusAxis,
    pub value: f32,
}

/// Identifies which of a [`Focus`]'s two coordinates a [`FocusRangeError`]
/// refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FocusAxis {
    X,
    Y,
}

impl std::fmt::Display for FocusRangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let axis = match self.axis {
            FocusAxis::X => "x",
            FocusAxis::Y => "y",
        };
        write!(
            f,
            "focus {axis} coordinate {} is out of the accepted range {FOCUS_MIN}..={FOCUS_MAX}",
            self.value
        )
    }
}

impl std::error::Error for FocusRangeError {}

impl Focus {
    /// Constructs a [`Focus`], rejecting either coordinate that falls
    /// outside `-1.0..=1.0` (Requirement 7.1, 7.4) instead of clamping or
    /// silently accepting it. `x` is checked before `y`, so a value that is
    /// invalid on both axes reports the `x` violation.
    pub fn new(x: f32, y: f32) -> Result<Self, FocusRangeError> {
        if !Self::in_range(x) {
            return Err(FocusRangeError {
                axis: FocusAxis::X,
                value: x,
            });
        }
        if !Self::in_range(y) {
            return Err(FocusRangeError {
                axis: FocusAxis::Y,
                value: y,
            });
        }
        Ok(Self { x, y })
    }

    /// Whether `value` falls within the accepted `-1.0..=1.0` range
    /// (inclusive on both ends). `NaN` is never in range, since every
    /// comparison against `NaN` is `false`.
    pub fn in_range(value: f32) -> bool {
        (FOCUS_MIN..=FOCUS_MAX).contains(&value)
    }

    /// The horizontal coordinate, always within `-1.0..=1.0`.
    pub fn x(&self) -> f32 {
        self.x
    }

    /// The vertical coordinate, always within `-1.0..=1.0`.
    pub fn y(&self) -> f32 {
        self.y
    }
}

/// The default focal point is the center, `(0.0, 0.0)` (Requirement 7.2:
/// "フォーカルポイントが指定されていないメディアの表現を返すとき... 規定
/// の既定値（中央）をフォーカルポイントとして返す").
impl Default for Focus {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0 }
    }
}

/// A pixel width/height pair plus its precomputed aspect ratio
/// (`width as f32 / height as f32`), used for both a [`Media`]'s original
/// dimensions and its thumbnail dimensions (Requirement 6.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
    pub aspect: f32,
}

/// Derived dimension metadata a completed processing job records
/// (Requirement 6.3): the original image's dimensions are always known
/// once processing succeeds, while the thumbnail's dimensions are `None`
/// until a small/preview derivative has actually been generated and
/// stored.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaMeta {
    pub original: Dimensions,
    pub small: Option<Dimensions>,
}

/// A single media attachment (design.md's `model` component sketch,
/// Requirements 1.2, 7.1).
///
/// `actor_id` is required (not `Option<Id>`) — see this module's doc
/// comment for why that alone satisfies Requirement 1.2's "所有アクターを
/// 必須とする" without any extra runtime validation. `meta`/`blurhash` are
/// only populated once a processing job completes successfully; they stay
/// `None` while `state == MediaState::Processing` and permanently `None`
/// if `state == MediaState::Failed`.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub id: Id,
    pub actor_id: Id,
    pub media_type: MediaType,
    pub state: MediaState,
    pub description: Option<String>,
    pub focus: Focus,
    pub meta: Option<MediaMeta>,
    pub blurhash: Option<String>,
    pub created_at: OffsetDateTime,
}

/// A queued/claimed/retried unit of asynchronous processing work for one
/// [`Media`] (design.md's `model` component sketch, Requirement 4.1).
///
/// `attempts` counts every claim of this job, including lease-expiry
/// reclaims (design.md's ワーカーによる派生物生成フロー note: "reclaim 自体
/// を 1 回の試行として `attempts` を加算する") — `ProcessingJobQueue` (task
/// 3.2) owns incrementing it. `locked_at` is `None` while the job is
/// unclaimed and set to the claiming worker's claim time while held.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessingJob {
    pub id: Id,
    pub media_id: Id,
    pub attempts: u32,
    pub run_at: OffsetDateTime,
    pub locked_at: Option<OffsetDateTime>,
    pub state: JobState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn focus_new_accepts_values_within_the_inclusive_range() {
        for (x, y) in [
            (0.0, 0.0),
            (-1.0, -1.0),
            (1.0, 1.0),
            (-1.0, 1.0),
            (1.0, -1.0),
            (0.5, -0.25),
        ] {
            let focus = Focus::new(x, y)
                .unwrap_or_else(|err| panic!("expected ({x}, {y}) to be accepted, got {err}"));
            assert_eq!(focus.x(), x);
            assert_eq!(focus.y(), y);
        }
    }

    #[test]
    fn focus_new_rejects_an_x_coordinate_outside_the_range() {
        let err = Focus::new(1.0001, 0.0).expect_err("expected x out of range to be rejected");
        assert_eq!(err.axis, FocusAxis::X);
        assert_eq!(err.value, 1.0001);
    }

    #[test]
    fn focus_new_rejects_a_y_coordinate_outside_the_range() {
        let err = Focus::new(0.0, -1.5).expect_err("expected y out of range to be rejected");
        assert_eq!(err.axis, FocusAxis::Y);
        assert_eq!(err.value, -1.5);
    }

    #[test]
    fn focus_new_reports_the_x_violation_when_both_axes_are_out_of_range() {
        let err = Focus::new(2.0, -2.0).expect_err("expected rejection");
        assert_eq!(err.axis, FocusAxis::X);
        assert_eq!(err.value, 2.0);
    }

    #[test]
    fn focus_new_rejects_nan_on_either_axis() {
        assert!(Focus::new(f32::NAN, 0.0).is_err());
        assert!(Focus::new(0.0, f32::NAN).is_err());
    }

    #[test]
    fn focus_in_range_matches_new_s_acceptance_boundary() {
        assert!(Focus::in_range(FOCUS_MIN));
        assert!(Focus::in_range(FOCUS_MAX));
        assert!(Focus::in_range(0.0));
        assert!(!Focus::in_range(FOCUS_MIN - f32::EPSILON));
        assert!(!Focus::in_range(FOCUS_MAX + f32::EPSILON));
        assert!(!Focus::in_range(f32::NAN));
    }

    #[test]
    fn focus_default_is_the_center() {
        let focus = Focus::default();
        assert_eq!(focus.x(), 0.0);
        assert_eq!(focus.y(), 0.0);
        // Also matches what `Focus::new` produces for the same coordinates.
        assert_eq!(focus, Focus::new(0.0, 0.0).unwrap());
    }

    #[test]
    fn focus_range_error_display_names_the_offending_axis_and_value() {
        let err = Focus::new(3.5, 0.0).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("x"));
        assert!(message.contains("3.5"));
    }

    fn sample_media(actor_id: Id) -> Media {
        Media {
            id: Id::from_i64(1),
            actor_id,
            media_type: MediaType::Image,
            state: MediaState::Processing,
            description: None,
            focus: Focus::default(),
            meta: None,
            blurhash: None,
            created_at: datetime!(2026-07-18 00:00:00 UTC),
        }
    }

    #[test]
    fn media_actor_id_is_a_required_non_optional_field() {
        // Exhaustive destructuring (no `..`) fails to compile if `actor_id`
        // were ever widened into `Option<Id>` or removed, structurally
        // proving Requirement 1.2's "所有アクターを必須とする" at the type
        // level rather than via a runtime check.
        let owner = Id::from_i64(42);
        let media = sample_media(owner);
        let Media {
            id: _,
            actor_id,
            media_type: _,
            state: _,
            description: _,
            focus: _,
            meta: _,
            blurhash: _,
            created_at: _,
        } = media;
        assert_eq!(actor_id, owner);
    }

    #[test]
    fn media_with_default_focus_reports_the_center() {
        let media = sample_media(Id::from_i64(7));
        assert_eq!(media.focus, Focus::default());
    }

    #[test]
    fn media_state_variants_are_distinct() {
        assert_ne!(MediaState::Processing, MediaState::Ready);
        assert_ne!(MediaState::Ready, MediaState::Failed);
        assert_ne!(MediaState::Processing, MediaState::Failed);
    }

    #[test]
    fn media_type_variants_are_distinct() {
        let all = [
            MediaType::Image,
            MediaType::Gifv,
            MediaType::Video,
            MediaType::Audio,
            MediaType::Unknown,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                assert_eq!(a == b, i == j);
            }
        }
    }

    #[test]
    fn job_state_variants_are_distinct() {
        assert_ne!(JobState::Queued, JobState::Processing);
        assert_ne!(JobState::Processing, JobState::Failed);
        assert_ne!(JobState::Queued, JobState::Failed);
    }

    #[test]
    fn processing_job_round_trips_its_fields() {
        let job = ProcessingJob {
            id: Id::from_i64(100),
            media_id: Id::from_i64(1),
            attempts: 2,
            run_at: datetime!(2026-07-18 00:05:00 UTC),
            locked_at: Some(datetime!(2026-07-18 00:01:00 UTC)),
            state: JobState::Processing,
        };
        assert_eq!(job.attempts, 2);
        assert_eq!(job.state, JobState::Processing);
        assert!(job.locked_at.is_some());
    }

    #[test]
    fn media_meta_holds_original_dimensions_and_optional_small() {
        let original = Dimensions {
            width: 1920,
            height: 1080,
            aspect: 1920.0 / 1080.0,
        };
        let small = Dimensions {
            width: 400,
            height: 225,
            aspect: 400.0 / 225.0,
        };
        let meta_without_small = MediaMeta {
            original,
            small: None,
        };
        assert!(meta_without_small.small.is_none());

        let meta_with_small = MediaMeta {
            original,
            small: Some(small),
        };
        assert_eq!(meta_with_small.small.unwrap().width, 400);
    }
}
