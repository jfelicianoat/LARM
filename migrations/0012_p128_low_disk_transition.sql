-- H09 keeps the last known level once per physical volume. Missing/offline or
-- unknown observations leave this row untouched, so they never invent a level.
CREATE TABLE storage_volume_alert_state (
  volume_key TEXT PRIMARY KEY CHECK(length(volume_key)>0),
  level TEXT NOT NULL CHECK(level IN ('NORMAL','WARNING','CRITICAL')),
  settings_revision INTEGER NOT NULL CHECK(settings_revision>=0),
  updated_at TEXT NOT NULL
) STRICT;

CREATE INDEX idx_notification_created_id ON notification(created_at,id);
