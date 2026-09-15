/**
 * Render contexts of this app's own pages (see `src/services/*.rs`).
 * `ssr<T>()` and `pageProps<T>()` take these as their type argument, so
 * template typos fail `astro check` instead of rendering a broken page.
 * Contexts of inherited pages are typed in the parent theme
 * (`@parent/types`, `@parent/types/pages`).
 */

export interface PublicProduct {
  id: number;
  name: string;
  slug: string;
  description: string;
  price: string;
}

export interface ProductsPage {
  products: PublicProduct[];
  search: string;
  page: number;
  total_pages: number;
  total_count: number;
  per_page: number;
}

export interface ProductDetailPage {
  product: PublicProduct;
  is_logged_in: boolean;
  ordered?: boolean;
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
