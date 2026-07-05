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

export interface UsersPage {
  rows: UserRow[];
}

export interface UserPage {
  id: number;
  email: string;
  role: string;
  roles: { value: string; label: string }[];
}

export interface SettingsPage {
  current_email: string;
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
}

export interface VerifyEmailChangeProps {
  verify_url: string;
}

export interface PasswordResetEmailProps {
  reset_url: string;
}
