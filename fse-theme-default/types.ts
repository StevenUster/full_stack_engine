/**
 * Render-context shapes for the generic `_model/*` pages — these mirror the
 * JSON the framework's generated handlers build (framework/src/models/
 * routes.rs). Apps overriding a page can import them via `@theme/types`.
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

export type ModelRow = Record<string, string | number | boolean | null>;

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
