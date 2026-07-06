-- Hand-written data migration (fse migrate only generates schema changes;
-- data seeds/backfills interleave with generated migrations by timestamp).
-- Default admin login: a@dm.in / change this password immediately.
INSERT INTO users (email, password, role, is_verified)
VALUES ('a@dm.in', '$argon2id$v=19$m=19456,t=2,p=1$/K9+8VqyybMd3davXrCBPQ$cs7f4L1PMOEeV8MDq5OzgCHWM6h1Nh0yxIvVbsahvk0', 'admin', 1);
