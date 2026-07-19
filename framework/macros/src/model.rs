//! Expansion of the `#[model(...)]` attribute macro.
//!
//! The struct is parsed into a [`fse_schema::TableDef`] (the exact code path
//! the ORM derive and the fse CLI use), then the macro arguments and the
//! field-level `#[ui(...)]` attributes are parsed and validated against it.
//! The emitted code is: the struct itself with `#[derive(Table, Debug,
//! Clone)]` attached (missing ones only, `#[ui]` stripped), the registration
//! (`TableDef` as JSON + const-constructed `UiModel`), and a typed
//! [`ModelResource`] implementation (see `resource.rs`) — all submitted to
//! the framework's `inventory` registry. All validation happens here, at
//! compile time.

use fse_schema::{ColumnDef, DefaultValue, SqlType, TableDef};
use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;

use crate::resource;

pub fn expand(args: TokenStream, item: &syn::ItemStruct) -> syn::Result<TokenStream> {
    let table = fse_schema::parse::table_from_struct(item, None)
        .map_err(|e| syn::Error::new(item.ident.span(), e.to_string()))?;
    if !table.auto_id() {
        return Err(syn::Error::new(
            item.ident.span(),
            "#[model] currently requires the conventional `id: i64` primary key — \
             custom and composite keys are planned",
        ));
    }

    let opts = model_opts(args, &table)?;
    let cols = collect_cols(item, &table)?;
    validate_form_coverage(item, &cols)?;
    validate_public_read(item, &opts, &cols)?;

    let emitted_struct = struct_with_derives(item);
    let resource_impl = resource::emit(&item.ident, &table, &opts, &cols);

    let table_json = serde_json::to_string(&table).map_err(|e| {
        syn::Error::new(
            item.ident.span(),
            format!("cannot serialize table metadata: {e}"),
        )
    })?;

    let permission = opt_str(opts.permission.as_deref());
    let path = opt_str(opts.path.as_deref());
    let public_read = opt_str(opts.public_read.as_deref());
    let title_field = opt_str(opts.title_field.as_deref());
    let (api, disabled) = (opts.api, opts.disabled);
    let (no_create, no_edit, no_delete) = (opts.no_create, opts.no_edit, opts.no_delete);

    let fields: Vec<TokenStream> = cols.iter().map(ui_field_tokens).collect();
    let n = fields.len();
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
                    fields: &__FSE_MODEL_UI_FIELDS,
                    form_fields: &[#(#form_fields),*],
                };

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

/// The struct as it will be emitted: `#[ui(...)]` attributes stripped (no
/// derive declares them once we're done expanding) and the standard derives
/// attached — `Table` for the ORM data layer, plus `Debug`/`Clone` for
/// convenience — skipping any the dev already wrote.
fn struct_with_derives(item: &syn::ItemStruct) -> TokenStream {
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
}

/// Parse and validate the `#[model(...)]` arguments.
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
                     disabled, no_create, no_edit, no_delete or title_field",
                ));
            }
            Ok(())
        });
        syn::parse::Parser::parse2(parser, args)?;
    }

    Ok(opts)
}

#[derive(Default)]
pub(crate) struct FieldUi {
    pub(crate) list: bool,
    pub(crate) search: bool,
    pub(crate) filter: bool,
    pub(crate) textarea: bool,
    pub(crate) readonly: bool,
    pub(crate) hidden: Option<bool>,
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
}

/// Parse and validate the field-level `#[ui(...)]` attributes into one
/// [`ColInfo`] per database column (relations carry no generated UI yet and
/// reject `#[ui]`).
fn collect_cols<'a>(
    item: &'a syn::ItemStruct,
    table: &'a TableDef,
) -> syn::Result<Vec<ColInfo<'a>>> {
    let syn::Fields::Named(struct_fields) = &item.fields else {
        unreachable!("table_from_struct already rejected non-named fields");
    };
    let field_by_name = |name: &str| {
        struct_fields
            .named
            .iter()
            .find(|f| f.ident.as_ref().is_some_and(|i| i == name))
            .expect("column parsed from this struct")
    };

    for rel in &table.relations {
        let field = field_by_name(&rel.field);
        if field.attrs.iter().any(|a| a.path().is_ident("ui")) {
            return Err(syn::Error::new(
                field.span(),
                "#[ui(...)] on a relation field is not supported yet — put it on the \
                 foreign-key column instead",
            ));
        }
    }

    let mut out = Vec::new();
    for col in &table.columns {
        let field = field_by_name(&col.name);
        let orm_text = has_orm_text_flag(field)?;
        let ui = field_ui(field, col, orm_text)?;
        // json/blob columns have no sensible generated rendering, so they
        // default to hidden unless the dev explicitly opts them in.
        let hidden = ui.hidden.unwrap_or(col.json || col.ty == SqlType::Blob);
        let in_form = !col.primary_key
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
        });
    }
    Ok(out)
}

/// Every NOT NULL column without a default must be fillable by the generated
/// create form, or inserts could never succeed — catch it at compile time.
fn validate_form_coverage(item: &syn::ItemStruct, cols: &[ColInfo]) -> syn::Result<()> {
    for c in cols {
        if !c.in_form && !c.def.primary_key && !c.def.nullable && c.def.default.is_none() {
            return Err(syn::Error::new(
                item.ident.span(),
                format!(
                    "column `{}` is NOT NULL without a default but excluded from generated \
                     forms (hidden/readonly/json/blob) — add #[orm(default = ...)], make it \
                     Option, or make it editable",
                    c.def.name
                ),
            ));
        }
    }
    Ok(())
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
    Ok(())
}

/// One `UiField` construction expression.
fn ui_field_tokens(c: &ColInfo) -> TokenStream {
    let name = &c.def.name;
    let (list, search, filter) = (c.ui.list, c.ui.search, c.ui.filter);
    let readonly = c.ui.readonly;
    let hidden = c.hidden;

    let is_select = c.def.is_enum && !c.orm_text;
    let widget = widget_variant(c.def, c.ui.textarea, is_select);
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
            widget: ::full_stack_engine::models::UiWidget::#widget,
            options: #options,
        }
    }
}

/// Parse one field's `#[ui(...)]` attributes and validate every flag against
/// the column's type.
fn field_ui(field: &syn::Field, col: &ColumnDef, orm_text: bool) -> syn::Result<FieldUi> {
    let mut ui = FieldUi::default();
    let plain_text = col.ty == SqlType::Text && !col.is_enum && !col.json;

    for attr in field.attrs.iter().filter(|a| a.path().is_ident("ui")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("list") {
                ui.list = true;
            } else if meta.path.is_ident("search") {
                if !plain_text || col.rust_type != "String" {
                    return Err(meta.error("search needs a plain text column (String)"));
                }
                ui.search = true;
            } else if meta.path.is_ident("filter") {
                if !((col.is_enum && !orm_text) || col.ty == SqlType::Boolean) {
                    return Err(
                        meta.error("filter needs a DbEnum or bool column to enumerate")
                    );
                }
                ui.filter = true;
            } else if meta.path.is_ident("textarea") {
                if !plain_text || col.rust_type != "String" {
                    return Err(meta.error("textarea needs a plain text column (String)"));
                }
                ui.textarea = true;
            } else if meta.path.is_ident("hidden") {
                ui.hidden = Some(true);
            } else if meta.path.is_ident("readonly") {
                ui.readonly = true;
            } else {
                return Err(meta.error(
                    "unknown #[ui(...)] key; expected list, search, filter, textarea, \
                     hidden or readonly",
                ));
            }
            Ok(())
        })?;
    }
    Ok(ui)
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
fn widget_variant(col: &ColumnDef, textarea: bool, is_select: bool) -> syn::Ident {
    let name = if col.json {
        "Json"
    } else if is_select {
        "Select"
    } else if col.is_enum {
        "Text"
    } else {
        match col.ty {
            SqlType::Integer | SqlType::Real => "Number",
            SqlType::Boolean => "Checkbox",
            SqlType::Timestamp => "DateTime",
            SqlType::Text if textarea => "Textarea",
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

fn opt_str(value: Option<&str>) -> TokenStream {
    match value {
        Some(s) => quote!(::core::option::Option::Some(#s)),
        None => quote!(::core::option::Option::None),
    }
}
