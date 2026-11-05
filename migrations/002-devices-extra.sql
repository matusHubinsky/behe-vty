-- 002-devices-extra.sql
ALTER TABLE devices ADD COLUMN protocol TEXT NOT NULL DEFAULT 'ssh';
ALTER TABLE devices ADD COLUMN vendor   TEXT NOT NULL DEFAULT 'cisco';
ALTER TABLE devices ADD COLUMN username TEXT NOT NULL DEFAULT '';