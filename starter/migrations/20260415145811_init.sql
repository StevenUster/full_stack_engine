CREATE TABLE
    users (
        id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
        email TEXT NOT NULL UNIQUE,
        password TEXT NOT NULL,
        role TEXT NOT NULL DEFAULT 'none',
        is_verified BOOLEAN NOT NULL DEFAULT 1,
        verification_token TEXT,
        reset_token TEXT,
        pending_email TEXT,
        email_change_token TEXT,
        created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL
    );

INSERT INTO users (email, password, role, is_verified) 
VALUES ('a@dm.in', '$argon2id$v=19$m=19456,t=2,p=1$/K9+8VqyybMd3davXrCBPQ$cs7f4L1PMOEeV8MDq5OzgCHWM6h1Nh0yxIvVbsahvk0', 'admin', 1);
