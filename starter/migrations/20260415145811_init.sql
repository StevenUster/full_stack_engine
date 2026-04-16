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
