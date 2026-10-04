/**
 * Render-context shapes for the generic `fse/*` pages — these mirror the
 * JSON the framework's generated handlers build (framework/src/models/
 * routes.rs). Child themes import them via `@parent/types` (or
 * `fse-theme-default/src/types`).
 */

export interface ModelColumn {
  name: string;
  widget:
    | "text"
    | "textarea"
    | "number"
    | "checkbox"
    | "datetime"
    | "date"
    | "select"
    | "relation"
    | "email"
    | "url"
    | "json";
  /** Enum values for `select`; `{ id, title }` rows the user may pick for
   * `relation` (null when they may not read the related model). */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  options: any[] | null;
  required: boolean;
  readonly: boolean;
  nullable: boolean;
  /** `#[ui(filter)]` kind, `null` when the column isn't a filter. */
  filter: "exact" | "contains" | "range" | null;
  /** Query params the filter reads: `[name]`, or `[name_from, name_to]`. */
  filter_params: string[];
  /** `#[ui(format = ...)]` kind, if any. */
  format: string | null;
  /** The row key to render: `{col}_display` when formatted, else `name`. */
  display: string;
  /** For a foreign key with `#[ui(show)]`: the row key holding `{ id, title }`. */
  relation: string | null;
}

/** A nested model or link table under a row (`{base_path}/{id}/{segment}`). */
export interface ModelChild {
  segment: string;
  table: string;
}

/** The parent row of a nested page. */
export interface ParentContext {
  meta: { table: string; base_path: string; title_field: string };
  row: ModelRow;
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
  /** `#[model(owner = ...)]` column — filled by the server, never a form field. */
  owner: string | null;
  /** `#[model(parent = ...)]` column of a nested model. */
  parent_column: string | null;
  /** Row actions (`#[model(actions(...))]`). */
  actions: string[];
  /** Nested models under each row. */
  children: ModelChild[];
  /** `children` or `links` non-empty. */
  has_subpages: boolean;
  /** Many-to-many link tables under each row. */
  links: ModelChild[];
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
  /** Current value per filter param (`""` = not filtered). */
  filters: Record<string, string>;
  /** Write permission, `can_create` hook and `no_create`, combined. */
  can_create: boolean;
  /** The parent row, for a nested model (else null). */
  parent: ParentContext | null;
}

export interface ModelFormPage {
  meta: ModelMetaContext;
  row: ModelRow;
  errors: { field: string; code: string }[];
  is_new: boolean;
  /** Write permission, `no_edit` and the model's `can_edit` hook, combined. */
  can_edit: boolean;
  /** Write permission, `no_delete` and the model's `can_delete` hook, combined. */
  can_delete: boolean;
  /** Row actions allowed on this row (`can_act`). */
  actions: string[];
  /** The parent row, for a nested model (else null). */
  parent: ParentContext | null;
}

/** `fse/links` — the rows of another model linked to one row. */
export interface ModelLinksPage {
  meta: ModelMetaContext;
  parent: ParentContext;
  link: { table: string; segment: string; other_table: string; base_path: string };
  linked: { id: number; title: string }[];
  can_write: boolean;
}

export interface ModelDetailPage {
  meta: ModelMetaContext;
  row: ModelRow;
}

/** One entry of the `nav` list the framework injects into every page. */
export interface NavItem {
  table: string;
  href: string;
  /** Already translated (`t.models.<table>.nav` / `.public_nav` / `.title`). */
  label: string;
  /** A `NavIcon` name. */
  icon: string;
  /** The public list's entry rather than the admin page. */
  public: boolean;
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
