# Artifacts and reviews

Artifacts are versioned documents stored by Loom for a user to read: designs,
reports, plans, diagrams, and the session goal. They are not worktree scratch
files and do not need to be committed to survive session archive.

Review is the staged feedback system shared by Artifacts and Changes. A review
draft stays private until one explicit submission freezes and delivers it to the
agent conversation.

## Artifact contract

An artifact belongs to a repository and optionally to a branch. Branch-scoped
and repo-shared artifacts use the same public name; a branch-scoped artifact
shadows a shared artifact of that name for the session. The immutable internal
artifact ID keeps history and reviews attached to the document that was actually
read even if a name is deleted, reused, or shadowed later.

Every write appends a revision. Reads use the latest revision by default and can
select an older one. Concurrent user and agent writes therefore create history
instead of overwriting a revision. Removing an artifact removes all of its
revisions; individual revisions are not pruned.

Markdown is the primary format. The renderer supports GFM and Mermaid and
projects these live reference forms:

- `#41` resolves an issue and renders its current ledger status.
- `artifact:design` links another artifact.
- `![Run output](artifact:screenshot)` embeds an image artifact in Markdown.

References inside code spans or blocks remain literal. Smartdoc projection is a
read-time join: the document references issues, while the issue ledger owns task
state. An embedded image resolves the latest artifact visible to that session,
using the same branch-scoped-before-repo-shared lookup as an artifact read. A
plan is simply an artifact convention; there is no plan parser or sync engine.

Pass a local image path directly to `loom artifacts write` (for example,
`loom artifacts write screenshot /tmp/result.png`). The CLI reads and snapshots
the image immediately into a bounded `image` artifact, so the stored artifact
does not depend on that filesystem path and remains available when the dashboard
or execution is remote. Another Markdown artifact can embed it with
`![Result](artifact:screenshot)`. Local paths embedded inside arbitrary Markdown
are deliberately not a storage contract; write the image itself rather than
hand-building base64.

### The goal artifact

The session goal is the branch-scoped artifact named `goal`. It is the editable,
versioned source read by session restart, handoff, summary, and metadata
assistance. `branches.goal` is a synchronized cache for list and search paths,
not a second independently editable owner. Historical rows without a goal
artifact retain the cache as a compatibility fallback.

### CLI and UI

Artifact syntax, actor policy, risk, REST paths, and MCP projections come from
the operation registry. Use `loom artifacts --help` for local syntax,
`loom help artifacts` for the operation index, or `GET /api/operations` for the
running server's machine-readable contract.

Session **Review → Artifacts** lists branch and shared documents, renders a
selected revision, and exposes source editing as a secondary mode. Saving an
edit creates a new revision. The panel can dock in the work area or open beside
Conversation/Agent without creating a global Artifacts destination.

Scratch is the inbound complement: Scratch is reference material the user gives
the agent; artifacts are documents the agent gives the user.

## Review contract

A review subject is exact and versioned:

- an artifact subject is the immutable artifact ID plus revision;
- the Changes subject is the session's bounded change-set version.

Listing a session's reviews returns submitted history plus the authenticated
creator's private draft. Creating a draft is recoverable: repeated creation for
the same creator and subject returns the existing draft rather than splitting
feedback across drafts.

### Draft

A draft contains an optional overall note and zero or more pending comments.
Artifact comments carry quote, prefix, suffix, block position, and revision
anchors; Changes comments carry stable old/new line coordinates from the typed
change set. Comments can be edited, deleted, and re-anchored before submission.

Every persisted mutation carries the current `expected_revision` and advances a
monotonic draft revision. A stale mutation returns the authoritative review so
the client can reload it for inspection instead of silently merging competing
edits. The captured subject version is provenance — quoted by delivery, gating
nothing. Staleness is a property of what a comment anchors at: decided where the
anchor renders, flagged when its lines hold changed text, and cleared by
re-anchoring.

Draft mutations are creator-private and emit no branch-wide event. Other tabs
owned by that creator refresh their draft when they regain focus. Text still in
an open comment editor is browser-local until saved; dock, pop, and route changes
that would lose it are blocked with a save-or-cancel instruction.

### Submit

`Submit review` performs one transaction:

1. Verify the optimistic draft revision.
2. Freeze the overall note and comments.
3. Render the exact delivery message on the server.
4. Record one `review_submitted` event.
5. Enqueue one delivery item.

A submitted review stays open to its reviewer: new comments attach to it
exactly like draft comments, thread replies queue past the delivered cursor,
and a changed overall note rides the next follow-up round — one submission
sends everything together. Any authenticated operator may resolve or reopen a
submitted comment or retry failed delivery; only the review's creator may
submit it, and only a draft may be discarded. A reply continues the
conversation wherever it left off — answering a resolved thread reopens it —
and a reply's author may revise or retract their own words until the reviewed
session has received them; delivered replies are history, like submitted
comment content.

ACP delivery enters a protected conversation inbox separate from retractable
operator prompts. Inbox consumption, journal delivery key, and live-turn claim
share one logical boundary so recovery cannot start a second turn for a review
already recorded in the journal. A newly submitted review replaces ordinary
live ACP work with one visible review turn instead of waiting behind an
unbounded agent turn; a standalone Stop still pauses automatic delivery, and a
retry never interrupts the review already in flight. Offline delivery remains
queued without consuming an attempt, and fenced leases prevent a stale worker
from regressing state. Terminal delivery is at-least-once.

If the reviewed artifact is later removed, submitted history remains readable by
stable review ID. Legacy artifact discussion threads are accepted only as
compatibility history; new feedback uses staged reviews.

The REST routes and storage ownership are catalogued in
[Architecture](ARCHITECTURE.md#rest-api). The agent workflow and reference
conventions live in [the smartdoc skill](../.agents/skills/smartdoc.md).

## Worktree file links

To share a file without copying it into the artifact store, run
`loom sessions url --path reports/index.md`. The command prints a public Loom
link of the form `/s/<session-id>/files/reports/index.md`, with path segments
URL-encoded. Paths are relative to the session's worktree and may name untracked
or ignored files. To link a file in another session, use
`loom sessions url <session> --path <path>`.
The same command shares images directly: `loom sessions url --path plots/loss.png`.
Preview selection and downloads use a shared MIME database rather than a list
of image extensions in Loom.

The file viewer shares Markdown, sandboxed HTML, and image previews with the
artifact viewer, and also displays plain text. Markdown images and
links resolve relative to the file. HTML previews accept self-contained pages,
as HTML artifacts do. Text previews are limited to 2 MiB; binary and larger text
files offer a download. File links show current worktree contents on load or
Reload, with no artifact revisions or comments. They require the same session
access as other Loom pages, reject paths outside the worktree (including
symlinks), and stop working if the file or worktree is removed.
