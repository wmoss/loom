import { computed, ref, watch, type ComputedRef, type Ref } from 'vue';
import type { Review } from '../types';
import { getMe } from '../api';
import type { ReviewDraftController } from './reviewDraftController';

/** The panels' shared contract for which review a reviewer's new words
 * attach to — the one place the follow-up-round selection lives.
 *
 * A review surface is one subject (a change-set, or one artifact) seen by
 * one operator: the open draft is the working copy; a submitted envelope
 * stays open to its creator, and the newest one that belongs to this
 * surface stands in wherever the draft is not. Comments, replies, and the
 * note all follow that one selection — the panel decides which envelopes
 * belong here (`envelopeScope`), the surface decides which of them is
 * open, and every mutation runs against exactly that choice.
 */
export interface ReviewSurface {
  /** The signed-in operator: only a review's creator may submit. */
  operator: Ref<string>;
  /** The open draft on this subject, if any. */
  draft: ComputedRef<Review | null>;
  /** The operator's submitted envelopes on this surface, newest first. */
  submittedEnvelopes: ComputedRef<Review[]>;
  /** The envelope a fresh composer comment or a tray note attaches to when
   * no draft is open: the newest submitted one, pending or not. */
  openEnvelope: ComputedRef<Review | null>;
  /** The follow-up surface: the envelope standing in wherever a draft is
   * not open. Its pending comments and replies queue beside a draft's —
   * and a note typed into its tray queues too, riding the next round. */
  continuation: ComputedRef<Review | null>;
  /** Installs a mutation result into the reviews list. */
  replaceReview(next: Review): void;
  /** Runs a pending-comment mutation against the surface that owns the
   * comment: the open draft through its controller (summary flush and
   * conflict reconciliation ride along), or the submitted review still
   * carrying it, whose undelivered comments are as editable as draft
   * ones. */
  mutateComment(run: (review: Review) => Promise<Review>): Promise<void>;
  /** Re-seeds the tray's note from the open envelope. */
  seedNote(): void;
}

/** Which submitted envelopes belong to the surface the tracked review is
 * picked from — all of the operator's by default; the changes panel narrows
 * to the snapshot scope it is viewing. */
type EnvelopeScope = (review: Review) => boolean;

/** The tracked review of one loaded list: the open draft, or — once the
 * signed-in operator is known — the newest envelope belonging to the
 * surface. The single pick both the live computeds and a load's
 * controller hydration go through. */
export function pickTrackedReview(
  reviews: Review[],
  operator: string,
  envelopeScope?: EnvelopeScope,
): Review | null {
  // The draft obeys the same scope the submitted envelopes do: a draft
  // opened on one snapshot never claims another scope's tray — its note,
  // its pending words, and its composer stay with the snapshot they were
  // written against, and the other scope opens its own review instead.
  const draft =
    reviews.find((review) => review.status === 'draft' && (envelopeScope?.(review) ?? true)) ??
    null;
  if (draft) return draft;
  if (!operator) return null;
  return (
    reviews
      .filter(
        (review) =>
          review.status === 'submitted' &&
          !review.legacy &&
          review.id > 0 &&
          review.created_by === operator &&
          (envelopeScope?.(review) ?? true),
      )
      .sort((a, b) => b.id - a.id)[0] ?? null
  );
}

export function useReviewSurface(options: {
  reviews: Ref<Review[]>;
  controller: ReviewDraftController<Review>;
  /** Unsaved-note gate: an envelope's note must not clobber a dirty tray. */
  summaryDirty: Ref<boolean>;
  /** Which submitted envelopes belong to this surface. */
  envelopeScope?: EnvelopeScope;
}): ReviewSurface {
  const { reviews, controller, summaryDirty, envelopeScope } = options;

  // The signed-in operator: only a review's creator may submit it, so the
  // continuation surface is gated on the name.
  const operator = ref('');
  void getMe()
    .then((me) => {
      operator.value = me.username ?? '';
    })
    .catch(() => {});

  // Scope-pinned like `pickTrackedReview`: a draft of another snapshot is
  // not this surface's draft. The tray composes against the tracked
  // surface, so a foreign draft here would parade its note and pending
  // words under a view they do not belong to.
  const draft = computed(
    () =>
      reviews.value.find(
        (review) => review.status === 'draft' && (envelopeScope?.(review) ?? true),
      ) ?? null,
  );

  /** The creator's submitted reviews on this surface, newest first — one
   * review carries the whole conversation, so these are the open
   * envelopes. */
  const submittedEnvelopes = computed(() =>
    reviews.value
      .filter(
        (review) =>
          review.status === 'submitted' &&
          !review.legacy &&
          review.id > 0 &&
          review.created_by === operator.value &&
          (envelopeScope?.(review) ?? true),
      )
      .sort((a, b) => b.id - a.id),
  );

  const openEnvelope = computed(() => submittedEnvelopes.value[0] ?? null);

  const continuation = computed(() => (draft.value ? null : openEnvelope.value));

  // The follow-up surface's note seeds the tray once the signed-in operator
  // is known: the envelope the reviewer already submitted carries the
  // summary they last wrote, and editing it queues for the next round.
  watch([openEnvelope, draft], seedNote);

  /** Seeds whenever the no-draft-with-envelope state is (re)entered: a
   * fresh operator arrival, a draft submitting into its envelope, or a
   * peer's submit that opened one mid-review. Also the explicit call a
   * submit makes once its freeze settles, so the microtask race with this
   * watch cannot leave the tray's note cleared. */
  function seedNote() {
    const envelope = openEnvelope.value;
    if (draft.value || !envelope || summaryDirty.value) return;
    controller.reconcile(envelope);
  }

  function replaceReview(next: Review) {
    const index = reviews.value.findIndex((review) => review.id === next.id);
    if (index < 0) reviews.value = [next, ...reviews.value];
    else reviews.value.splice(index, 1, next);
  }

  async function mutateComment(run: (review: Review) => Promise<Review>): Promise<void> {
    if (draft.value) {
      await controller.command((current) => {
        if (!current) throw new Error('Review draft is unavailable.');
        return run(current);
      });
      return;
    }
    const target = continuation.value;
    if (!target) throw new Error('Review draft is unavailable.');
    replaceReview(await run(target));
  }

  return {
    operator,
    draft,
    submittedEnvelopes,
    openEnvelope,
    continuation,
    replaceReview,
    mutateComment,
    seedNote,
  };
}
