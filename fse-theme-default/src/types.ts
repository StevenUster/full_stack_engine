/**
 * Render-context shapes for the generic `_model/*` pages — these mirror the
 * JSON the framework's generated handlers build (framework/src/models/
 * routes.rs). Child themes import them via `@parent/types` (or
 * `fse-theme-default/src/types`).
 */

export interface ModelColumn {
  name: string;
  widget: "text" | "textarea" | "number" | "checkbox" | "datetime" | "select" | "json";
  options: string[] | null;
  required: boolean;
  readonly: boolean;
  nullable: boolean;
}

export interface ModelMetaContext {
  table: string;
  base_path: string;
  can_write: boolean;
  no_create: boolean;
  no_edit: boolean;
  no_delete: boolean;
  public_read: string | null;
  title_field: string;
  list_columns: ModelColumn[];
  form_columns: ModelColumn[];
  search_columns: string[];
  filter_columns: ModelColumn[];
}

/** One row — column values are only known at runtime, hence `any`. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type ModelRow = Record<string, any>;

export interface ModelListPage {
  meta: ModelMetaContext;
  rows: ModelRow[];
  total: number;
  page: number;
  per_page: number;
  total_pages: number;
  has_prev: boolean;
  has_next: boolean;
  prev_page: number;
  next_page: number;
  search: string | null;
  sort: string | null;
  desc: boolean;
  filters: Record<string, string>;
}

export interface ModelFormPage {
  meta: ModelMetaContext;
  row: ModelRow;
  errors: { field: string; code: string }[];
  is_new: boolean;
}

export interface ModelDetailPage {
  meta: ModelMetaContext;
  row: ModelRow;
}

/** One entry of the `nav` list the framework injects into every page. */
export interface NavItem {
  table: string;
  href: string;
}

/** The signed-in user as injected by the framework (absent when signed out). */
export interface SessionUser {
  id: number;
  role: string;
  is_admin: boolean;
  can_read_users: boolean;
}

/** Context keys every page receives, whatever renders it. */
export interface ShellContext {
  nav: NavItem[];
  user?: SessionUser;
  lang_prefix: string;
}
