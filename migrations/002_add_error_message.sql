-- Track the last fetch error message per feed (used by scheduler/queue status).
ALTER TABLE feeds ADD COLUMN error_message TEXT;
