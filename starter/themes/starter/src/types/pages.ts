/**
 * Render contexts of this app's own pages (generated ones, and the custom
 * flows in `src/services/*.rs`).
 * `ssr<T>()` and `pageProps<T>()` take these as their type argument, so
 * template typos fail `astro check` instead of rendering a broken page.
 * Contexts of inherited pages are typed in the parent theme
 * (`@parent/types`, `@parent/types/pages`).
 */

/**
 * A product row as the generated public pages (`public_read`) render it:
 * the visible columns plus `price_display` / `created_at_display` from
 * `#[ui(format = ...)]`.
 */
export interface PublicProduct {
  id: number;
  name: string;
  slug: string;
  description: string | null;
  price: number;
  price_display: string;
  created_at_display: string;
}

/** `products` — the generated public list (published products only). */
export interface ProductsPage {
  rows: PublicProduct[];
  search: string;
  page: number;
  total_pages: number;
  total_count: number;
  per_page: number;
}

/** `products/detail` — the generated public detail page. */
export interface ProductDetailPage {
  row: PublicProduct;
  /** Present when someone is signed in (injected on every page). */
  user?: { id: number };
}

export interface MyOrderRow {
  id: number;
  quantity: number;
  note: string;
  status: string;
  created_at: string;
  product_name: string;
  product_link: string;
}

export interface MyOrdersPage {
  rows: MyOrderRow[];
}
