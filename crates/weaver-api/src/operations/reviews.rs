//! Creator-private draft feedback that, once submitted, becomes a durable
//! review delivered into the reviewed session's own conversation — and stays
//! open to its reviewer: new comments and a changed overall note ride follow-up
//! rounds through the same submission.
//!
//! Mutating operations (`comments.delete`, `comments.update`, `discard`,
//! `update`) are limited to the review's own creator. Delivered comments are
//! history — reply or resolve them — while `discard` stays draft-only.

use super::registry::OperationSpec;
use super::OperationBundle;

pub(super) use super::prelude;
pub mod comments {
    //! Anchored feedback comments on a review: added and edited while it is a
    //! private draft, resolved once it has been submitted, and — from the
    //! reviewed session's own credential — answered after submit.
    pub(super) use super::prelude;
    pub mod create {
        use super::prelude::*;

        /// Append an anchored feedback comment to a draft review.
        ///
        /// A review's comments stay private to the operator drafting them until
        /// `reviews.submit` delivers the whole thing.
        #[operation(id = "reviews.comments.create", actor = User, scope = Global, risk = Write,
                    default = custom)]
        pub struct Input {
            /// The review to comment on.
            #[operand(positional)]
            pub id: i64,
            /// Optimistic-concurrency guard on the review's draft revision.
            pub expected_revision: i64,
            /// The subject version (artifact revision, or change-set version) the
            /// anchor was taken against.
            pub subject_version: String,
            #[operand(json)]
            pub anchor_kind: ReviewAnchorKindDto,
            #[operand(json)]
            pub anchor: ReviewAnchorDto,
            pub body: String,
        }

        impl Default for Input {
            fn default() -> Self {
                Self {
                    id: 0,
                    expected_revision: 0,
                    subject_version: String::new(),
                    anchor_kind: ReviewAnchorKindDto::Text,
                    anchor: ReviewAnchorDto::Text(ArtifactTextAnchorDto {
                        quote: String::new(),
                        prefix: String::new(),
                        suffix: String::new(),
                        block_index: None,
                    }),
                    body: String::new(),
                }
            }
        }

        pub type Output = ReviewDto;
    }

    pub mod delete {
        use super::prelude::*;

        /// Remove a draft review comment.
        #[operation(id = "reviews.comments.delete", actor = User, scope = Global, risk = Write,
                    render = custom, cli = "review delete-comment")]
        pub struct Input {
            /// The review the comment belongs to.
            #[operand(positional)]
            pub id: i64,
            /// The comment to delete.
            #[operand(positional)]
            pub comment_id: i64,
            /// Optimistic-concurrency guard on the review's draft revision, as
            /// `loom review ls` or the previous mutation reported it.
            #[operand(long = "revision")]
            pub expected_revision: i64,
        }

        pub type Output = ReviewDto;
    }

    pub mod resolve {
        use super::prelude::*;

        /// Mark a comment on a submitted review resolved or unresolved.
        ///
        /// Any operator may resolve a comment on any submitted review — unlike the
        /// other `reviews.comments.*` operations, this one is not limited to the
        /// creator.
        #[operation(id = "reviews.comments.resolve", actor = User, scope = Global, risk = Write)]
        pub struct Input {
            /// The submitted review the comment belongs to.
            #[operand(positional)]
            pub id: i64,
            /// The comment to resolve or unresolve.
            #[operand(positional)]
            pub comment_id: i64,
            pub resolved: bool,
        }

        pub type Output = ReviewCommentDto;
    }

    pub mod reply {
        use super::prelude::*;

        /// Append one entry to a comment's reply thread.
        ///
        /// Reachable from both sides of the review loop: the reviewed
        /// session's own credential (as itself, its own reviews only) and
        /// any operator (by username, any submitted review — the reach
        /// `comments.resolve` grants). A comment's replies form one flat,
        /// oldest-first conversation; resolving stays with `resolve`.
        #[operation(id = "reviews.comments.reply", actor = SessionSelf, scope = Session, risk = Write,
                    grants = ["loom/reviews/write@v1"])]
        pub struct Input {
            /// The comment whose thread the reply joins, as `reviews.list`
            /// reports it.
            #[operand(positional)]
            pub comment_id: i64,
            /// The reply body.
            pub body: String,
            #[operand(context)]
            pub session: String,
        }

        pub type Output = ReviewCommentDto;
    }

    pub mod update {
        use super::prelude::*;

        /// Edit a draft review comment's text, or replace its anchor.
        ///
        /// Replacing the anchor requires `subject_version`, `anchor_kind`, and
        /// `anchor` together — a partial anchor replacement is rejected.
        #[operation(id = "reviews.comments.update", actor = User, scope = Global, risk = Write)]
        pub struct Input {
            /// The review the comment belongs to.
            #[operand(positional)]
            pub id: i64,
            /// The comment to update.
            #[operand(positional)]
            pub comment_id: i64,
            /// Optimistic-concurrency guard on the review's draft revision.
            pub expected_revision: i64,
            pub body: Option<String>,
            /// The subject version the replacement anchor was taken against.
            /// Required together with `anchor_kind` and `anchor`.
            pub subject_version: Option<String>,
            #[operand(json, default = None)]
            pub anchor_kind: Option<ReviewAnchorKindDto>,
            #[operand(json, default = None)]
            pub anchor: Option<ReviewAnchorDto>,
        }

        pub type Output = ReviewDto;
    }
}

pub mod create {
    use super::prelude::*;

    /// Create or reuse a draft review over a session's artifact or its
    /// change-set, seeding it against the currently-visible subject version.
    #[operation(id = "reviews.create", actor = User, scope = Session, risk = Write,
                default = custom)]
    pub struct Input {
        /// The session whose artifact or change-set is under review.
        #[operand(positional)]
        pub session: String,
        pub subject_kind: ReviewSubjectKindDto,
        /// Artifact name for `subject_kind = "artifact"`, or `"changes"` for
        /// `subject_kind = "changes"`.
        pub subject_key: String,
        /// The subject version this draft starts from: an artifact revision
        /// number, or the current change-set version (which must match exactly
        /// for a changes review).
        pub subject_version: String,
    }

    impl Default for Input {
        fn default() -> Self {
            Self {
                session: String::new(),
                subject_kind: ReviewSubjectKindDto::Artifact,
                subject_key: String::new(),
                subject_version: String::new(),
            }
        }
    }

    pub type Output = ReviewDto;
}

pub mod discard {
    use super::prelude::*;

    /// Permanently discard a draft review.
    #[operation(id = "reviews.discard", actor = User, scope = Global, risk = Destructive,
                render = custom, cli = "review discard")]
    pub struct Input {
        /// The draft review to discard.
        #[operand(positional)]
        pub id: i64,
        /// Optimistic-concurrency guard on the review's draft revision, as
        /// `loom review ls` or the previous mutation reported it.
        #[operand(long = "revision")]
        pub expected_revision: i64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    pub struct Output {
        pub discarded: bool,
    }
}

pub mod get {
    use super::prelude::*;

    /// Fetch a durable review by id, refreshed against its subject's current
    /// version.
    ///
    /// A submitted review is visible to any operator; a draft only to the
    /// operator who created it.
    #[operation(id = "reviews.get", actor = User, scope = Global, risk = Read,
                cli = "review show")]
    pub struct Input {
        /// The review to fetch.
        #[operand(positional)]
        pub id: i64,
    }

    pub type Output = ReviewDto;
}

pub mod list {
    use super::prelude::*;

    /// List a session's reviews for one subject — an artifact or its change-set.
    ///
    /// Reachable by both the reviewed session's own credential and a human
    /// operator: sessions may see submitted feedback on their own work, but not
    /// draft reviews from other operators. The delivered review prompt is
    /// compact; this is where the reviewed session reads the full anchors.
    #[operation(id = "reviews.list", actor = SessionSelf, scope = Session, risk = Read,
                grants = ["loom/reviews/read@v1"], default = custom)]
    pub struct Input {
        pub subject_kind: ReviewSubjectKindDto,
        /// The artifact name for `subject_kind = "artifact"`, or `"changes"` for
        /// `subject_kind = "changes"`.
        pub subject_key: String,
        #[operand(context)]
        pub session: String,
    }

    impl Default for Input {
        fn default() -> Self {
            Self {
                subject_kind: ReviewSubjectKindDto::Artifact,
                subject_key: String::new(),
                session: String::new(),
            }
        }
    }

    pub type Output = Vec<ReviewDto>;
}

pub mod retry_delivery {
    use super::prelude::*;

    /// Retry a submitted review's delivery after it failed.
    ///
    /// Any operator may retry delivery of any submitted review — unlike
    /// `reviews.submit`, this one is not limited to the creator.
    #[operation(id = "reviews.retry_delivery", actor = User, scope = Global, risk = Write,
                render = custom, cli = "review retry")]
    pub struct Input {
        /// The submitted review whose delivery failed.
        #[operand(positional)]
        pub id: i64,
    }

    pub type Output = ReviewDto;
}

pub mod submit {
    use super::prelude::*;

    /// Submit a review's draft, delivering its structured feedback into the
    /// reviewed session's own conversation.
    ///
    /// Limited to the review's own creator.
    #[operation(id = "reviews.submit", actor = User, scope = Global, risk = Write)]
    pub struct Input {
        /// The review to submit.
        #[operand(positional)]
        pub id: i64,
        /// Optimistic-concurrency guard on the review's draft revision.
        pub expected_revision: i64,
    }

    pub type Output = ReviewDto;
}

pub mod update {
    use super::prelude::*;

    /// Edit a review's overall note. On a draft it becomes the submitted
    /// note; on the reviewer's own submitted review it queues for the next
    /// follow-up round, like a pending comment does.
    #[operation(id = "reviews.update", actor = User, scope = Global, risk = Write)]
    pub struct Input {
        /// The review to update.
        #[operand(positional)]
        pub id: i64,
        /// Optimistic-concurrency guard on the review's draft revision.
        pub expected_revision: i64,
        pub summary: Option<String>,
    }

    pub type Output = ReviewDto;
}

static OPERATIONS: &[&OperationSpec] = &[
    get::SPEC,
    update::SPEC,
    discard::SPEC,
    list::SPEC,
    create::SPEC,
    comments::create::SPEC,
    comments::update::SPEC,
    comments::delete::SPEC,
    comments::resolve::SPEC,
    comments::reply::SPEC,
    submit::SPEC,
    retry_delivery::SPEC,
];

pub(super) const fn bundle() -> OperationBundle {
    OperationBundle {
        name: "reviews",
        label: "Reviews",
        operations: OPERATIONS,
    }
}
