//! Expansion of the `#[model(...)]` attribute macro.
//!
//! The struct is parsed into a [`fse_schema::TableDef`] (the exact code path
//! the ORM derive and the fse CLI use), then the macro arguments and the
//! field-level `#[ui(...)]` attributes are parsed and validated against it.
//! The emitted code is: the struct itself with `#[derive(Table, Debug,
//! Clone)]` attached (missing ones only, `#[ui]` stripped), the registration
//! (`TableDef` as JSON + const-constructed `UiModel`), and a typed
//! `ModelResource` implementation (see `resource.rs`) — or, for a
//! `#[model(link = ...)]` join table, a `LinkResource` (see `link.rs`) — all
//! submitted to the framework's `inventory` registry. Everything one struct
//! can know is validated here, at compile time; what needs the other models
//! (parents, relation targets) is checked at boot by `models::check()`.

use fse_schema::{ColumnDef, DefaultValue, SqlType, TableDef};
use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;

use crate::{link, resource};

pub fn expand(args: TokenStream, item: &syn::ItemStruct) -> syn::Result<TokenStream> {
    let table = fse_schema::parse::table_from_struct(item, None)
        .map_err(|e| syn::Error::new(item.ident.span(), e.to_string()))?;
    let opts = model_opts(args, &table)?;
    if opts.link.is_some() {
        return link::expand(item, &table, &opts);
    }
    if !table.auto_id() {
        return Err(syn::Error::new(
            item.ident.span(),
            "#[model] needs the conventional `id: i64` primary key — for a many-to-many \
             join table with a composite key use #[model(link = owner_column)]",
        ));
    }

    let cols = collect_cols(item, &table, &opts)?;
    let rels = collect_relations(item, &table)?;
    let creatable = validate_form_coverage(item, &opts, &cols)?;
    validate_public_read(item, &opts, &cols)?;
    let title_field = resolve_title(item, &opts, &cols)?;

    let emitted_struct = struct_with_derives(item);
    let resource_impl = resource::emit(&item.ident, &table, &opts, &cols, creatable, &title_field);
    let ident = &item.ident;
    let default_hooks = default_hooks(ident, opts.hooks);

    let table_json = table_json(item, &table)?;
    let permission = opt_str(opts.permission.as_deref());
    let path = opt_str(opts.path.as_deref());
    let public_read = opt_str(opts.public_read.as_deref());
    let owner = opt_str(opts.owner.as_deref());
    let parent = opt_str(opts.parent.as_deref());
    let order_by = match &opts.order_by {
        Some((col, desc)) => quote!(::core::option::Option::Some((#col, #desc))),
        None => quote!(::core::option::Option::None),
    };
    let per_page = match opts.per_page {
        Some(n) => quote!(::core::option::Option::Some(#n)),
        None => quote!(::core::option::Option::None),
    };
    let actions: Vec<String> = opts.actions.iter().map(ToString::to_string).collect();
    let (api, disabled) = (opts.api, opts.disabled);
    let (no_create, no_edit, no_delete) = (opts.no_create, opts.no_edit, opts.no_delete);

    let fields: Vec<TokenStream> = cols.iter().map(ui_field_tokens).collect();
    let n = fields.len();
    let relations: Vec<TokenStream> = rels.iter().map(relation_tokens).collect();
    let n_rel = relations.len();
    let form_fields: Vec<&str> = cols
        .iter()
        .filter(|c| c.in_form)
        .map(|c| c.def.name.as_str())
        .collect();

    Ok(quote! {
        #emitted_struct

        const _: () = {
            static __FSE_MODEL_UI_FIELDS: [::full_stack_engine::models::UiField; #n] =
                [#(#fields),*];
            static __FSE_MODEL_UI_RELATIONS: [::full_stack_engine::models::UiRelation; #n_rel] =
                [#(#relations),*];
            static __FSE_MODEL_UI: ::full_stack_engine::models::UiModel =
                ::full_stack_engine::models::UiModel {
                    permission: #permission,
                    path: #path,
                    public_read: #public_read,
                    api: #api,
                    disabled: #disabled,
                    no_create: #no_create,
                    no_edit: #no_edit,
                    no_delete: #no_delete,
                    title_field: #title_field,
                    owner: #owner,
                    parent: #parent,
                    order_by: #order_by,
                    per_page: #per_page,
                    actions: &[#(#actions),*],
                    relations: &__FSE_MODEL_UI_RELATIONS,
                    fields: &__FSE_MODEL_UI_FIELDS,
                    form_fields: &[#(#form_fields),*],
                };

            #default_hooks

            #resource_impl

            ::full_stack_engine::inventory::submit! {
                ::full_stack_engine::models::ModelRegistration {
                    table_json: #table_json,
                    ui: &__FSE_MODEL_UI,
                    resource: &__FSE_MODEL_RESOURCE,
                }
            }
        };
    })
}

/// `hooks` = the app writes `impl ModelHooks`; otherwise every hook is the
/// trait default. Writing the impl without the flag is a "conflicting
/// implementations" error, never a silently ignored impl.
pub(crate) fn default_hooks(ident: &syn::Ident, hooks: bool) -> TokenStream {
    if hooks {
        quote!()
    } else {
        quote!(impl ::full_stack_engine::models::ModelHooks for #ident {})
    }
}

pub(crate) fn table_json(item: &syn::ItemStruct, table: &TableDef) -> syn::Result<String> {
    serde_json::to_string(table).map_err(|e| {
        syn::Error::new(
            item.ident.span(),
            format!("cannot serialize table metadata: {e}"),
        )
    })
}

/// The struct as it will be emitted: `#[ui(...)]` attributes stripped (no
/// derive declares them once we're done expanding) and the standard derives
/// attached — `Table` for the ORM data layer, plus `Debug`/`Clone` for
/// convenience — skipping any the dev already wrote.
pub(crate) fn struct_with_derives(item: &syn::ItemStruct) -> TokenStream {
    let mut item = item.clone();
    for field in &mut item.fields {
        field.attrs.retain(|a| !a.path().is_ident("ui"));
    }

    let mut derives: Vec<TokenStream> = Vec::new();
    if !fse_schema::parse::has_derive(&item.attrs, "Table") {
        derives.push(quote!(::full_stack_engine::prelude::Table));
    }
    if !fse_schema::parse::has_derive(&item.attrs, "Debug") {
        derives.push(quote!(::core::fmt::Debug));
    }
    if !fse_schema::parse::has_derive(&item.attrs, "Clone") {
        derives.push(quote!(::core::clone::Clone));
    }

    if derives.is_empty() {
        quote!(#item)
    } else {
        quote! {
            #[derive(#(#derives),*)]
            #item
        }
    }
}

#[derive(Default)]
pub(crate) struct ModelOpts {
    pub(crate) permission: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) public_read: Option<String>,
    pub(crate) api: bool,
    pub(crate) disabled: bool,
    pub(crate) no_create: bool,
    pub(crate) no_edit: bool,
    pub(crate) no_delete: bool,
    pub(crate) title_field: Option<String>,
    pub(crate) owner: Option<String>,
    pub(crate) hooks: bool,
    pub(crate) parent: Option<String>,
    /// `(column, descending)`.
    pub(crate) order_by: Option<(String, bool)>,
    pub(crate) per_page: Option<i64>,
    pub(crate) actions: Vec<syn::Ident>,
    pub(crate) link: Option<String>,
}

/// A foreign-key column the macro can name: `i64`/`Option<i64>`, with
/// `references(...)`.
fn fk_column<'a>(table: &'a TableDef, column: &str, what: &str) -> Result<&'a ColumnDef, String> {
    let Some(col) = table.column(column) else {
        return Err(format!(
            "{what} column `{column}` is not a column of {}",
            table.struct_name
        ));
    };
    if col.primary_key && table.auto_id() {
        return Err(format!(
            "{what} column `{column}` cannot be the primary key"
        ));
    }
    if col.rust_type != "i64" || col.json {
        return Err(format!(
            "{what} column `{column}` must be a plain `i64` (or `Option<i64>`) id column"
        ));
    }
    Ok(col)
}

/// Parse and validate the `#[model(...)]` arguments.
#[allow(clippy::too_many_lines)]
fn model_opts(args: TokenStream, table: &TableDef) -> syn::Result<ModelOpts> {
    let mut opts = ModelOpts::default();
    if args.is_empty() {
        return Ok(opts);
    }

    {
        let parser = syn::meta::parser(|meta| {
            if meta.path.is_ident("permission") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                if lit.value().is_empty() {
                    return Err(meta.error("permission must not be empty"));
                }
                opts.permission = Some(lit.value());
            } else if meta.path.is_ident("path") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                let value = lit.value();
                if value.is_empty() {
                    return Err(meta.error("path must not be empty"));
                }
                if value.starts_with('/') || value.ends_with('/') {
                    return Err(meta.error(
                        "path is a bare segment relative to the site root — drop the slash, \
                         e.g. path = \"product-manager\"",
                    ));
                }
                opts.path = Some(value);
            } else if meta.path.is_ident("public_read") {
                let column = if meta.input.peek(syn::Token![=]) {
                    let ident: syn::Ident = meta.value()?.parse()?;
                    ident.to_string()
                } else {
                    table.primary_key()[0].name.clone()
                };
                let Some(col) = table.column(&column) else {
                    return Err(meta.error(format!(
                        "public_read column `{column}` is not a column of {}",
                        table.struct_name
                    )));
                };
                if !col.unique && !col.primary_key {
                    return Err(meta.error(format!(
                        "public_read column `{column}` must be unique (or the primary key) \
                         so a row has one stable public URL"
                    )));
                }
                opts.public_read = Some(column);
            } else if meta.path.is_ident("title_field") {
                let ident: syn::Ident = meta.value()?.parse()?;
                let column = ident.to_string();
                if table.column(&column).is_none() {
                    return Err(meta.error(format!(
                        "title_field `{column}` is not a column of {}",
                        table.struct_name
                    )));
                }
                opts.title_field = Some(column);
            } else if meta.path.is_ident("owner") {
                let ident: syn::Ident = meta.value()?.parse()?;
                let column = ident.to_string();
                fk_column(table, &column, "owner").map_err(|e| meta.error(e))?;
                opts.owner = Some(column);
            } else if meta.path.is_ident("parent") {
                let ident: syn::Ident = meta.value()?.parse()?;
                let column = ident.to_string();
                let col = fk_column(table, &column, "parent").map_err(|e| meta.error(e))?;
                if col.references.is_none() || col.nullable {
                    return Err(meta.error(format!(
                        "parent column `{column}` must be a NOT NULL foreign key: \
                         #[orm(references(Parent, on_delete = cascade))] pub {column}: i64"
                    )));
                }
                opts.parent = Some(column);
            } else if meta.path.is_ident("link") {
                let ident: syn::Ident = meta.value()?.parse()?;
                let column = ident.to_string();
                let col = fk_column(table, &column, "link").map_err(|e| meta.error(e))?;
                if col.references.is_none() {
                    return Err(meta.error(format!(
                        "link column `{column}` must be a foreign key: \
                         #[orm(references(Owner, on_delete = cascade))]"
                    )));
                }
                opts.link = Some(column);
            } else if meta.path.is_ident("order_by") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                let raw = lit.value();
                let (column, desc) = match raw.strip_prefix('-') {
                    Some(c) => (c.to_string(), true),
                    None => (raw.clone(), false),
                };
                match table.column(&column) {
                    Some(c) if !c.json => {}
                    _ => {
                        return Err(meta.error(format!(
                            "order_by `{raw}` must name a column of {} (prefix `-` for \
                             descending, e.g. order_by = \"-created_at\")",
                            table.struct_name
                        )));
                    }
                }
                opts.order_by = Some((column, desc));
            } else if meta.path.is_ident("per_page") {
                let lit: syn::LitInt = meta.value()?.parse()?;
                let n: i64 = lit.base10_parse()?;
                if !(1..=100).contains(&n) {
                    return Err(meta.error("per_page must be between 1 and 100"));
                }
                opts.per_page = Some(n);
            } else if meta.path.is_ident("actions") {
                meta.parse_nested_meta(|action| {
                    let Some(ident) = action.path.get_ident() else {
                        return Err(
                            action.error("an action is a method name, e.g. actions(publish)")
                        );
                    };
                    if opts.actions.iter().any(|a| a == ident) {
                        return Err(action.error("duplicate action"));
                    }
                    opts.actions.push(ident.clone());
                    Ok(())
                })?;
            } else if meta.path.is_ident("hooks") {
                opts.hooks = true;
            } else if meta.path.is_ident("api") {
                opts.api = true;
            } else if meta.path.is_ident("disabled") {
                opts.disabled = true;
            } else if meta.path.is_ident("no_create") {
                opts.no_create = true;
            } else if meta.path.is_ident("no_edit") {
                opts.no_edit = true;
            } else if meta.path.is_ident("no_delete") {
                opts.no_delete = true;
            } else {
                return Err(meta.error(
                    "unknown #[model(...)] key; expected permission, path, public_read, api, \
                     disabled, no_create, no_edit, no_delete, title_field, owner, parent, \
                     order_by, per_page, actions(...), link or hooks",
                ));
            }
            Ok(())
        });
        syn::parse::Parser::parse2(parser, args)?;
    }

    if opts.owner.is_some() && opts.owner == opts.parent {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "owner and parent cannot be the same column",
        ));
    }
    if opts.link.is_some()
        && (opts.public_read.is_some()
            || opts.api
            || opts.owner.is_some()
            || opts.parent.is_some()
            || !opts.actions.is_empty()
            || opts.order_by.is_some())
    {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "a link table only takes path and hooks besides link = ...",
        ));
    }

    Ok(opts)
}

/// `#[ui(filter)]` / `#[ui(filter = range)]` — mirrors the framework's
/// `models::UiFilter`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterKind {
    Exact,
    Contains,
    Range,
}

#[derive(Default)]
pub(crate) struct FieldUi {
    pub(crate) list: bool,
    pub(crate) search: bool,
    pub(crate) filter: Option<FilterKind>,
    pub(crate) textarea: bool,
    pub(crate) readonly: bool,
    pub(crate) hidden: Option<bool>,
    pub(crate) required: bool,
    pub(crate) private: bool,
    pub(crate) email: bool,
    pub(crate) url: bool,
    pub(crate) min: Option<f64>,
    pub(crate) max: Option<f64>,
    pub(crate) slug_from: Option<String>,
    pub(crate) format: Option<String>,
}

/// Everything the emission passes know about one database column: its
/// schema definition, the field's Rust type with `Option` stripped, and the
/// resolved UI configuration.
pub(crate) struct ColInfo<'a> {
    pub(crate) def: &'a ColumnDef,
    pub(crate) inner_ty: syn::Type,
    pub(crate) orm_text: bool,
    pub(crate) ui: FieldUi,
    pub(crate) hidden: bool,
    /// In the generated create/edit forms — and therefore bound by the
    /// generated `create`/`update` code.
    pub(crate) in_form: bool,
    /// The `owner = ...` column.
    pub(crate) is_owner: bool,
    /// The `parent = ...` column.
    pub(crate) is_parent: bool,
}

/// Secret-looking column names are hidden from every generated page and
/// JSON response unless nothing else would make sense — never shown, never
/// listed, never a form field.
fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "password"
        || n.ends_with("_password")
        || n == "token"
        || n.ends_with("_token")
        || n.contains("secret")
        || n.ends_with("_hash")
        || n == "api_key"
        || n.ends_with("_api_key")
}

fn field_by_name<'a>(item: &'a syn::ItemStruct, name: &str) -> &'a syn::Field {
    let syn::Fields::Named(struct_fields) = &item.fields else {
        unreachable!("table_from_struct already rejected non-named fields");
    };
    struct_fields
        .named
        .iter()
        .find(|f| f.ident.as_ref().is_some_and(|i| i == name))
        .expect("column parsed from this struct")
}

/// Parse and validate the field-level `#[ui(...)]` attributes into one
/// [`ColInfo`] per database column.
pub(crate) fn collect_cols<'a>(
    item: &'a syn::ItemStruct,
    table: &'a TableDef,
    opts: &ModelOpts,
) -> syn::Result<Vec<ColInfo<'a>>> {
    let mut out = Vec::new();
    for col in &table.columns {
        let field = field_by_name(item, &col.name);
        let orm_text = has_orm_text_flag(field)?;
        let ui = field_ui(field, col, orm_text, table)?;
        let secret = is_secret_name(&col.name);
        if secret && (ui.list || ui.search || ui.filter.is_some() || ui.hidden == Some(false)) {
            return Err(syn::Error::new(
                field.span(),
                format!(
                    "`{}` looks like a secret, so generated pages never show it — drop the \
                     #[ui] display flags (or rename the column if it isn't a secret)",
                    col.name
                ),
            ));
        }
        // json/blob columns have no sensible generated rendering and secrets
        // must never render, so all three default to hidden.
        let hidden = ui.hidden.unwrap_or(false) || secret || col.json || col.ty == SqlType::Blob;
        let is_owner = opts.owner.as_deref() == Some(col.name.as_str());
        let is_parent = opts.parent.as_deref() == Some(col.name.as_str());
        // The owner and parent columns are filled by the framework (from the
        // signed-in user / the URL), never from the submission — a form
        // field for either would let users file rows under someone else.
        let in_form = !col.primary_key
            && !is_owner
            && !is_parent
            && !hidden
            && !ui.readonly
            && !col.json
            && col.ty != SqlType::Blob
            && col.default != Some(DefaultValue::Now);
        out.push(ColInfo {
            def: col,
            inner_ty: option_inner(&field.ty).clone(),
            orm_text,
            ui,
            hidden,
            in_form,
            is_owner,
            is_parent,
        });
    }
    Ok(out)
}

/// One `#[orm(relation = fk)]` field and its `#[ui]` flags.
pub(crate) struct RelInfo {
    pub(crate) field: String,
    pub(crate) column: String,
    pub(crate) target: String,
    pub(crate) show: bool,
    pub(crate) list: bool,
    pub(crate) with: Vec<String>,
}

fn collect_relations(item: &syn::ItemStruct, table: &TableDef) -> syn::Result<Vec<RelInfo>> {
    let mut out = Vec::new();
    for rel in &table.relations {
        let field = field_by_name(item, &rel.field);
        let mut info = RelInfo {
            field: rel.field.clone(),
            column: rel.local_column.clone(),
            target: rel.target_struct.clone(),
            show: false,
            list: false,
            with: Vec::new(),
        };
        for attr in field.attrs.iter().filter(|a| a.path().is_ident("ui")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("show") {
                    info.show = true;
                    if meta.input.peek(syn::token::Paren) {
                        meta.parse_nested_meta(|deeper| {
                            let Some(ident) = deeper.path.get_ident() else {
                                return Err(deeper.error("expected a relation field name"));
                            };
                            info.with.push(ident.to_string());
                            Ok(())
                        })?;
                    }
                } else if meta.path.is_ident("list") {
                    info.show = true;
                    info.list = true;
                } else {
                    return Err(meta.error(
                        "on a relation field #[ui(...)] takes show, show(relation, ...) or list",
                    ));
                }
                Ok(())
            })?;
        }
        out.push(info);
    }
    Ok(out)
}

/// Every NOT NULL column without a default must be fillable by the generated
/// create — from the form, the signed-in user (`owner`) or the URL
/// (`parent`) — or inserts could never succeed. Where the model mounts a
/// create endpoint that's a compile error; otherwise (`disabled`,
/// `no_create`) no create is generated. Returns whether create is possible.
fn validate_form_coverage(
    item: &syn::ItemStruct,
    opts: &ModelOpts,
    cols: &[ColInfo],
) -> syn::Result<bool> {
    let uncovered = cols.iter().find(|c| {
        !c.in_form
            && !c.def.primary_key
            && !c.is_owner
            && !c.is_parent
            && !c.def.nullable
            && c.def.default.is_none()
    });
    let Some(c) = uncovered else {
        return Ok(true);
    };
    if opts.disabled || opts.no_create {
        return Ok(false);
    }
    Err(syn::Error::new(
        item.ident.span(),
        format!(
            "column `{}` is NOT NULL without a default but excluded from generated forms \
             (hidden/readonly/secret/json/blob) — add #[orm(default = ...)], make it Option, \
             make it editable, or add no_create",
            c.def.name
        ),
    ))
}

/// The `public_read` column must be usable as a URL key.
fn validate_public_read(
    item: &syn::ItemStruct,
    opts: &ModelOpts,
    cols: &[ColInfo],
) -> syn::Result<()> {
    let Some(name) = &opts.public_read else {
        return Ok(());
    };
    let col = cols
        .iter()
        .find(|c| &c.def.name == name)
        .expect("validated against the table in model_opts");
    if col.def.json || col.orm_text || col.def.ty == SqlType::Blob {
        return Err(syn::Error::new(
            item.ident.span(),
            format!(
                "public_read column `{name}` cannot be used as a URL key — use a plain \
                 unique column (text, number, uuid)"
            ),
        ));
    }
    if col.hidden {
        return Err(syn::Error::new(
            item.ident.span(),
            format!("public_read column `{name}` is hidden — a public URL key must be visible"),
        ));
    }
    Ok(())
}

/// The title column: `title_field`, else the first visible plain text
/// column, else the primary key. Never a hidden column.
fn resolve_title(
    item: &syn::ItemStruct,
    opts: &ModelOpts,
    cols: &[ColInfo],
) -> syn::Result<String> {
    if let Some(name) = &opts.title_field {
        let col = cols
            .iter()
            .find(|c| &c.def.name == name)
            .expect("validated");
        if col.hidden {
            return Err(syn::Error::new(
                item.ident.span(),
                format!("title_field `{name}` is hidden (or looks like a secret)"),
            ));
        }
        return Ok(name.clone());
    }
    Ok(cols
        .iter()
        .filter(|c| !c.hidden)
        .find(|c| c.def.ty == SqlType::Text && !c.def.is_enum && !c.def.json)
        .or_else(|| cols.iter().find(|c| c.def.primary_key))
        .map(|c| c.def.name.clone())
        .expect("a model has a primary key"))
}

/// One `UiField` construction expression.
fn ui_field_tokens(c: &ColInfo) -> TokenStream {
    let name = &c.def.name;
    let (list, search) = (c.ui.list, c.ui.search);
    let filter = match c.ui.filter {
        None => quote!(::core::option::Option::None),
        Some(kind) => {
            let variant = syn::Ident::new(
                match kind {
                    FilterKind::Exact => "Exact",
                    FilterKind::Contains => "Contains",
                    FilterKind::Range => "Range",
                },
                proc_macro2::Span::call_site(),
            );
            quote!(::core::option::Option::Some(::full_stack_engine::models::UiFilter::#variant))
        }
    };
    let readonly = c.ui.readonly;
    let hidden = c.hidden;
    let private = c.ui.private;
    let required = c.is_required();
    let format = opt_str(c.ui.format.as_deref());

    let is_select = c.def.is_enum && !c.orm_text;
    let widget = widget_variant(c, is_select);
    let options = if is_select {
        let inner = &c.inner_ty;
        quote! {
            ::core::option::Option::Some(|| {
                <#inner>::VARIANTS.iter().map(|v| v.as_str()).collect()
            })
        }
    } else {
        quote!(::core::option::Option::None)
    };

    quote! {
        ::full_stack_engine::models::UiField {
            name: #name,
            list: #list,
            search: #search,
            filter: #filter,
            readonly: #readonly,
            hidden: #hidden,
            private: #private,
            required: #required,
            format: #format,
            widget: ::full_stack_engine::models::UiWidget::#widget,
            options: #options,
        }
    }
}

fn relation_tokens(r: &RelInfo) -> TokenStream {
    let (field, column, target) = (&r.field, &r.column, &r.target);
    let (show, list) = (r.show, r.list);
    let with = &r.with;
    quote! {
        ::full_stack_engine::models::UiRelation {
            field: #field,
            column: #column,
            target: #target,
            show: #show,
            list: #list,
            with: &[#(#with),*],
        }
    }
}

impl ColInfo<'_> {
    /// The form rejects an empty value: NOT NULL without a default, or
    /// `#[ui(required)]`. Checkboxes are never "required".
    pub(crate) fn is_required(&self) -> bool {
        // A `slug_from` column is filled by the server when left empty.
        self.def.ty != SqlType::Boolean
            && self.ui.slug_from.is_none()
            && (self.ui.required || (!self.def.nullable && self.def.default.is_none()))
    }

    /// The foreign-key target struct, when this column references one.
    pub(crate) fn references(&self) -> Option<&str> {
        self.def.references.as_ref().map(|fk| fk.table.as_str())
    }
}

/// Parse one field's `#[ui(...)]` attributes and validate every flag against
/// the column's type.
#[allow(clippy::too_many_lines)]
fn field_ui(
    field: &syn::Field,
    col: &ColumnDef,
    orm_text: bool,
    table: &TableDef,
) -> syn::Result<FieldUi> {
    let mut ui = FieldUi::default();
    let plain_text = col.ty == SqlType::Text && !col.is_enum && !col.json;
    let string = plain_text && col.rust_type == "String";
    let numeric = matches!(col.ty, SqlType::Integer | SqlType::Real) && !col.is_enum;

    for attr in field.attrs.iter().filter(|a| a.path().is_ident("ui")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("list") {
                ui.list = true;
            } else if meta.path.is_ident("search") {
                if !string {
                    return Err(meta.error("search needs a plain text column (String)"));
                }
                ui.search = true;
            } else if meta.path.is_ident("filter") {
                let kind = if meta.input.peek(syn::Token![=]) {
                    let ident: syn::Ident = meta.value()?.parse()?;
                    match ident.to_string().as_str() {
                        "exact" => FilterKind::Exact,
                        "contains" => FilterKind::Contains,
                        "range" => FilterKind::Range,
                        _ => {
                            return Err(syn::Error::new(
                                ident.span(),
                                "filter kind must be exact, contains or range",
                            ));
                        }
                    }
                } else {
                    default_filter(col, orm_text).ok_or_else(|| {
                        meta.error(
                            "this column type has no default filter — use a DbEnum, bool, \
                             text, number, date or timestamp column",
                        )
                    })?
                };
                check_filter(col, orm_text, kind).map_err(|msg| meta.error(msg))?;
                ui.filter = Some(kind);
            } else if meta.path.is_ident("textarea") {
                if !string {
                    return Err(meta.error("textarea needs a plain text column (String)"));
                }
                ui.textarea = true;
            } else if meta.path.is_ident("hidden") {
                ui.hidden = Some(true);
            } else if meta.path.is_ident("readonly") {
                ui.readonly = true;
            } else if meta.path.is_ident("private") {
                ui.private = true;
            } else if meta.path.is_ident("required") {
                if col.ty == SqlType::Boolean {
                    return Err(meta.error("a checkbox cannot be required"));
                }
                ui.required = true;
            } else if meta.path.is_ident("email") {
                if !string {
                    return Err(meta.error("email needs a plain text column (String)"));
                }
                ui.email = true;
            } else if meta.path.is_ident("url") {
                if !string {
                    return Err(meta.error("url needs a plain text column (String)"));
                }
                ui.url = true;
            } else if meta.path.is_ident("min") || meta.path.is_ident("max") {
                if !(numeric || string) {
                    return Err(meta.error(
                        "min/max need a number column (value) or a String column (length)",
                    ));
                }
                let lit: syn::Lit = meta.value()?.parse()?;
                let value = match &lit {
                    syn::Lit::Int(i) => i.base10_parse::<f64>()?,
                    syn::Lit::Float(f) => f.base10_parse::<f64>()?,
                    _ => return Err(meta.error("min/max take a number, e.g. min = 1")),
                };
                if meta.path.is_ident("min") {
                    ui.min = Some(value);
                } else {
                    ui.max = Some(value);
                }
            } else if meta.path.is_ident("slug_from") {
                if !string {
                    return Err(meta.error("slug_from needs a String column"));
                }
                let ident: syn::Ident = meta.value()?.parse()?;
                let source = ident.to_string();
                match table.column(&source) {
                    Some(c) if c.rust_type == "String" && c.ty == SqlType::Text && !c.is_enum => {}
                    _ => {
                        return Err(syn::Error::new(
                            ident.span(),
                            format!("slug_from = {source}: `{source}` must be a String column"),
                        ));
                    }
                }
                ui.slug_from = Some(source);
            } else if meta.path.is_ident("format") {
                let ident: syn::Ident = meta.value()?.parse()?;
                let kind = ident.to_string();
                let temporal = col.ty == SqlType::Timestamp
                    || (col.ty == SqlType::Text
                        && matches!(col.rust_type.as_str(), "String" | "NaiveDate" | "NaiveTime"));
                let ok = match kind.as_str() {
                    "date" | "datetime" | "time" => temporal,
                    "number" | "currency" => numeric,
                    _ => {
                        return Err(syn::Error::new(
                            ident.span(),
                            "format must be date, datetime, time, number or currency",
                        ));
                    }
                };
                if !ok {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("format = {kind} doesn't fit this column's type"),
                    ));
                }
                ui.format = Some(kind);
            } else {
                return Err(meta.error(
                    "unknown #[ui(...)] key; expected list, search, filter, filter = \
                     exact|contains|range, textarea, hidden, private, readonly, required, email, url, \
                     min = n, max = n, slug_from = column or format = \
                     date|datetime|time|number|currency",
                ));
            }
            Ok(())
        })?;
    }
    if let (Some(min), Some(max)) = (ui.min, ui.max)
        && min > max
    {
        return Err(syn::Error::new(field.span(), "min is larger than max"));
    }
    Ok(ui)
}

/// The filter `#[ui(filter)]` picks from the column type: a dropdown for
/// enums and bools, substring match for text, bounds for numbers and
/// dates/times.
fn default_filter(col: &ColumnDef, orm_text: bool) -> Option<FilterKind> {
    if col.json || orm_text {
        return None;
    }
    if col.is_enum || col.ty == SqlType::Boolean {
        return Some(FilterKind::Exact);
    }
    match col.ty {
        SqlType::Text if col.rust_type == "String" => Some(FilterKind::Contains),
        SqlType::Integer | SqlType::Real | SqlType::Timestamp => Some(FilterKind::Range),
        SqlType::Text if matches!(col.rust_type.as_str(), "NaiveDate" | "NaiveTime") => {
            Some(FilterKind::Range)
        }
        _ => None,
    }
}

/// Whether an explicitly chosen filter kind works for the column.
fn check_filter(col: &ColumnDef, orm_text: bool, kind: FilterKind) -> Result<(), &'static str> {
    if col.json || orm_text || col.ty == SqlType::Blob {
        return Err("json, blob and #[orm(text)] columns cannot be filtered");
    }
    match kind {
        FilterKind::Exact => Ok(()),
        FilterKind::Contains if col.rust_type == "String" && !col.is_enum => Ok(()),
        FilterKind::Contains => Err("filter = contains needs a plain text column (String)"),
        FilterKind::Range if col.is_enum || col.ty == SqlType::Boolean => {
            Err("filter = range needs a number, date, time or text column")
        }
        // Text ranges compare lexicographically — right for ISO dates kept
        // as strings.
        FilterKind::Range => Ok(()),
    }
}

/// Whether the field carries `#[orm(text)]` — stored as TEXT via
/// `as_str()`/`FromStr` but *not* a `DbEnum`, so it has no `VARIANTS` to
/// offer as select options and no `sqlx` binding for dynamic conditions.
fn has_orm_text_flag(field: &syn::Field) -> syn::Result<bool> {
    for attr in field.attrs.iter().filter(|a| a.path().is_ident("orm")) {
        let metas = attr.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        )?;
        if metas.iter().any(|m| m.path().is_ident("text")) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The default widget for a column, as a `UiWidget` variant ident.
fn widget_variant(c: &ColInfo, is_select: bool) -> syn::Ident {
    let col = c.def;
    let name = if col.json {
        "Json"
    } else if is_select {
        "Select"
    } else if col.is_enum {
        "Text"
    } else if c.references().is_some() {
        "Relation"
    } else if c.ui.email {
        "Email"
    } else if c.ui.url {
        "Url"
    } else {
        match col.ty {
            SqlType::Integer | SqlType::Real => "Number",
            SqlType::Boolean => "Checkbox",
            SqlType::Timestamp => "DateTime",
            SqlType::Text if c.ui.textarea => "Textarea",
            SqlType::Text if col.rust_type == "NaiveDate" => "Date",
            SqlType::Text | SqlType::Blob => "Text",
        }
    };
    syn::Ident::new(name, proc_macro2::Span::call_site())
}

/// `Option<T>` → `T`, anything else unchanged.
fn option_inner(ty: &syn::Type) -> &syn::Type {
    if let syn::Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
        && seg.ident == "Option"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        inner
    } else {
        ty
    }
}

pub(crate) fn opt_str(value: Option<&str>) -> TokenStream {
    match value {
        Some(s) => quote!(::core::option::Option::Some(#s)),
        None => quote!(::core::option::Option::None),
    }
}
