-- The exact upload time a lookup gave (yt-dlp's `timestamp`, Unix seconds). It replaces a guessed
-- channel list date on the video's channel_videos row, now and whenever the video is seen later.
ALTER TABLE videos ADD COLUMN uploaded_at INTEGER;
