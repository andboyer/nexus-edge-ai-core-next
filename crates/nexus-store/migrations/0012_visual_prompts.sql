-- M3.1: visual prompts for the YOLOE detector backend.
--
-- One uploaded reference crop becomes one row in `visual_prompts`. The
-- engine encodes the crop once at admin time (engine process, NOT a
-- worker — encoder weights stay co-located with the admin HTTP
-- handler) and persists the resulting embedding here. Workers later
-- load it for every camera that has the prompt attached and feed it
-- into the per-frame YOLOE visual-prompt session.
--
-- Hard invariants enforced here:
--
--   1. **`visual_prompts.name` is UNIQUE.** The operator-supplied
--      label surfaces directly as `Detection.label` and from there
--      into CEL rule expressions (`object.label == 'amazon_van'`).
--      Two prompts named the same would render the rule ambiguous,
--      so the constraint must live at the schema layer where every
--      future writer is forced through it.
--
--   2. **Both join FKs are `ON DELETE CASCADE`.** Deleting a
--      camera or a visual prompt cleans up the join table without
--      orphan rows. The Rust `delete_visual_prompt` adds a
--      higher-level guard that returns `Conflict` if any camera is
--      still attached (so an accidental admin click can't silently
--      wipe the join across N cameras) — but the cascade is the
--      ultimate safety net.
--
--   3. **`embedding_blob` is opaque to SQL.** The engine validates
--      `embedding_blob.len() == embedding_dim * 4` on read (f32
--      little-endian). Storing as BLOB sidesteps SQLite's TEXT
--      coercion entirely.
--
--   4. **`encoder_model_id` lets the engine detect stale
--      embeddings.** When the encoder model changes (new YOLOE
--      release, retraining), existing embeddings are still readable
--      but won't score against the new detector. The engine logs a
--      warn-once on mismatch and (in a follow-up) re-encodes lazily.
--
--   5. **`created_at` / `updated_at` use the lex-sortable ISO
--      form**, NOT bare `CURRENT_TIMESTAMP`. SQLite's default
--      emits `YYYY-MM-DD HH:MM:SS` (space, no T, no TZ) which
--      lex-compares LESS than every chrono RFC3339-bound string
--      `YYYY-MM-DDTHH:MM:SS.fff+00:00` — so any range query that
--      binds RFC3339 cutoffs would exclude default-inserted rows.
--      `strftime('%Y-%m-%dT%H:%M:%fZ', 'now')` keeps both sides
--      in the same total order. See user-memory note for the
--      forensic on this trap.

CREATE TABLE visual_prompts (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Operator-typed handle. Surfaces verbatim as Detection.label.
    name              TEXT    NOT NULL UNIQUE,
    -- Optional free-text description shown in the admin UI grid.
    description       TEXT,
    -- Path RELATIVE to runtime.visual_prompts_dir. Engine joins
    -- the two at read time so operators can move the directory
    -- (e.g. onto a faster SSD partition) without rewriting rows.
    image_path        TEXT    NOT NULL,
    -- SHA-256 hex of the original image bytes. Used to detect
    -- duplicate uploads and as the disambiguator in the on-disk
    -- filename (`<id>_<sha8>.<ext>`).
    image_sha256      TEXT    NOT NULL,
    -- f32 little-endian embedding produced by the image_encoder
    -- ONNX. Length == embedding_dim * 4; the Rust loader
    -- validates this on every read.
    embedding_blob    BLOB    NOT NULL,
    embedding_dim     INTEGER NOT NULL,
    -- Stable id for the encoder that produced `embedding_blob`
    -- (e.g. `yoloe26_s_image_encoder`). Used to detect stale
    -- embeddings when the encoder model rolls forward.
    encoder_model_id  TEXT    NOT NULL,
    created_at        TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at        TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (embedding_dim > 0),
    CHECK (length(embedding_blob) = embedding_dim * 4)
);

CREATE TABLE camera_visual_prompts (
    camera_id         INTEGER NOT NULL,
    visual_prompt_id  INTEGER NOT NULL,
    -- Free text printed in the admin "attached to N cameras"
    -- view. Defaults to NULL; reserved for the M3.2 "per-camera
    -- override label" surface.
    note              TEXT,
    attached_at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (camera_id, visual_prompt_id),
    FOREIGN KEY (camera_id)        REFERENCES cameras(id)        ON DELETE CASCADE,
    FOREIGN KEY (visual_prompt_id) REFERENCES visual_prompts(id) ON DELETE CASCADE
);

-- Reverse lookup: "which cameras have this prompt attached?" runs
-- on every delete to drive the Conflict check in
-- `Store::delete_visual_prompt`.
CREATE INDEX idx_camera_visual_prompts_visual
    ON camera_visual_prompts (visual_prompt_id);
