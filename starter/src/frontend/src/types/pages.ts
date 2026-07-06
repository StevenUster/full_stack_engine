/**
 * Shapes of the render contexts each Rust service passes to its page
 * (see `src/services/*.rs`). `ssr<T>()` and `pageProps<T>()` take these as
 * their type argument, so template typos fail `astro check` instead of
 * rendering a broken page.
 */

/** Column definition for the Table component (page-static, lives in TS). */
export interface TableHeader {
  /** Column heading shown to the user. */
  title: string;
  /** Row field this column renders. */
  key: string;
  format?: "number" | "delete" | "image";
}

export interface UserRow {
  id: number;
  email: string;
  role: string;
  created_at: string;
  link: string;
  delete_url: string;
}

export interface Role {
  value: string;
  label: string;
}

export interface UsersPage {
  rows: UserRow[];
  search: string;
  filter_role: string;
  roles: Role[];
  page: number;
  total_pages: number;
  total_count: number;
  per_page: number;
}

export interface UserPage {
  id: number;
  email: string;
  role: string;
  roles: Role[];
}

export interface SettingsPage {
  current_email: string;
  first_name: string;
  last_name: string;
  success?: string;
  error?: string;
  email_success?: string;
  email_error?: string;
}

export interface LoginPage {
  error?: string;
  success?: string;
}

export interface RegisterPage {
  first_name?: string;
  last_name?: string;
  email?: string;
  error?: string;
}

export interface ForgotPasswordPage {
  /** Set to any value when the reset mail was (reportedly) sent. */
  success?: string;
  error?: string;
}

export interface ResetPasswordPage {
  token: string;
  error?: string;
  success?: string;
}

export interface ErrorPage {
  error: string;
}

export interface VerifyEmailProps {
  verify_url: string;
  base_url: string;
}

export interface VerifyEmailChangeProps {
  verify_url: string;
  base_url: string;
}

export interface PasswordResetEmailProps {
  reset_url: string;
  base_url: string;
}

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

export interface ProductManagerRow {
  id: number;
  name: string;
  price: string;
  status: string;
  created_at: string;
  link: string;
  delete_url: string;
}

export interface ProductManagerPage {
  rows: ProductManagerRow[];
  search: string;
  page: number;
  total_pages: number;
  total_count: number;
  per_page: number;
}

export interface ProductCreatePage {
  name: string;
  slug: string;
  description: string;
  price: string;
  error_slug: string;
}

export interface Product {
  id: number;
  name: string;
  slug: string;
  description: string;
  price: string;
  status: string;
}

export interface ProductDetailsManagerPage {
  product: Product;
  error_slug: string;
}

export interface OrderRow {
  id: number;
  quantity: number;
  note: string;
  status: string;
  created_at: string;
  user_email: string;
  fulfill_url: string;
  delete_url: string;
}

export interface ProductOrdersPage {
  product: { id: number; name: string };
  rows: OrderRow[];
  search: string;
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
