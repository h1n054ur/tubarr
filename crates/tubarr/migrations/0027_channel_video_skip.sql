-- Why a video can never be an episode, from what the feed or channel list showed: a YouTube Short,
-- members only, Premium only, private. No matcher looks at it, so it ends up ignored. NULL for
-- every other video. A later channel list that shows it open clears the reason.
ALTER TABLE channel_videos ADD COLUMN skip_reason TEXT;
