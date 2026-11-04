-- A video marked for members or Premium only is looked up again later, since many channels open
-- such an upload to everyone after a while. Marks made before this carry the "video unavailable"
-- wording with yt-dlp's members-only message: they get the new wording and the retry flag.
UPDATE unplayable_videos
    SET reason = 'video is for members or Premium only: ' || substr(reason, length('video unavailable: ') + 1),
        retry = 1
    WHERE reason LIKE 'video unavailable: %'
      AND (reason LIKE '%members-only%'
        OR reason LIKE '%join this channel to get access%'
        OR reason LIKE '%only available to music premium members%'
        OR reason LIKE '%only available to youtube premium members%');
