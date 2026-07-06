-- Example manageable resource: a catalog product with a simple publish
-- lifecycle. `draft` products are hidden from the public catalog; only
-- `published` ones are. `archived` products are hidden again (e.g.
-- discontinued) but kept for historical orders.
CREATE TABLE
    products (
        id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
        name TEXT NOT NULL,
        slug TEXT NOT NULL UNIQUE,
        description TEXT,
        price REAL NOT NULL DEFAULT 0,
        status TEXT NOT NULL DEFAULT 'draft',
        created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL
    );
