-- A user placing an order for a product. `status` starts at `pending` and is
-- moved to `fulfilled` (or `cancelled`) by a manager from the product-manager
-- orders tab.
CREATE TABLE
    orders (
        id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
        product_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        quantity INTEGER NOT NULL DEFAULT 1,
        note TEXT,
        status TEXT NOT NULL DEFAULT 'pending',
        created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL,
        FOREIGN KEY (product_id) REFERENCES products (id) ON DELETE CASCADE,
        FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
    );

CREATE INDEX idx_orders_product ON orders (product_id);

CREATE INDEX idx_orders_user ON orders (user_id);
