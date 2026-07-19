/**
 * Shared page-context types for the theme's auth/admin pages —
 * mirrors what the framework's auth module renders.
 */

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
