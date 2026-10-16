-- Per-session runtime placement override, stamped once at create. Blank uses
-- the deployment's configured runner (LOOM_RUNNER); 'sibling' places the
-- session's supervisor as a sibling of its launcher's supervisor — inside the
-- launching session's own container under the docker runner, reusing its
-- placement instead of creating a new one.
ALTER TABLE sessions ADD COLUMN runner TEXT NOT NULL DEFAULT '';
