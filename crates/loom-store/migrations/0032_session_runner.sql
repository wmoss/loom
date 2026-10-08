-- Per-session runtime placement override, stamped once at create. Blank uses
-- the deployment's configured runner (LOOM_RUNNER); 'local' runs this
-- session's supervisor directly beside the loom server process instead — no
-- container isolation or memory cgroup, the cheap placement for a trusted
-- helper such as a code-review subagent.
ALTER TABLE sessions ADD COLUMN runner TEXT NOT NULL DEFAULT '';
