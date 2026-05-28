-- 0017_motion_clips_frame_size.sql — record the per-clip
-- supervisor-frame (analysis frame) dimensions on the
-- motion_clips row.
--
-- Until v0.1.19 every clip was recorded against a hardcoded
-- 960×540 supervisor frame, so the UI overlay scaled bboxes from
-- motion_events (which are in supervisor-frame coords) against
-- that constant. The per-camera supervisor-frame work introduced
-- in this migration's PR derives the dims from the camera's
-- resolved detector input size (640 / 960 / 1280 → 640×360 /
-- 960×540 / 1280×720, 16:9 native, even-rounded), so the
-- correct overlay scale is per-clip.
--
-- Historical rows: default to (960, 540) so the existing UI
-- overlay continues to work without a back-fill migration of
-- every clip on every edge install.
--
-- New rows: the engine's GstClipRecorder / StubClipRecorder
-- stamp the actual supervisor dims at clip-open time via
-- NewClip.frame_width / frame_height.

ALTER TABLE motion_clips
    ADD COLUMN frame_width INTEGER NOT NULL DEFAULT 960;
ALTER TABLE motion_clips
    ADD COLUMN frame_height INTEGER NOT NULL DEFAULT 540;
