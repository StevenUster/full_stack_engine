//! Expansion of `#[derive(Model)]`.
//!
//! The struct is parsed into a [`fse_schema::TableDef`] (the exact code path
//! the ORM derive and the fse CLI use), then the `#[model(...)]` /
//! `#[ui(...)]` attributes are parsed and validated against it. The emitted
//! code is only a registration: the `TableDef` as a JSON literal plus a
//! const-constructed `UiModel`, submitted to the framework's `inventory`
//! registry. All validation happens here, at compile time.

use fse_schema::{ColumnDef, SqlType, TableDef};
use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;

pub fn expand(item: &syn::ItemStruct) -> syn::Result<TokenStream> {
    let table = fse_schema::parse::table_from_struct(item, None)
        .map_err(|e| syn::Error::new(item.ident.span(), e.to_string()))?;

    let opts = model_opts(item, &table)?;
    let fields = ui_fields(item, &table)?;

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

    let n = fields.len();
    Ok(quote! {
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
                };
            ::full_stack_engine::inventory::submit! {
                ::full_stack_engine::models::ModelRegistration {
                    table_json: #table_json,
                    ui: &__FSE_MODEL_UI,
                }
            }
        };
    })
}

#[derive(Default)]
struct ModelOpts {
    permission: Option<String>,
    path: Option<String>,
    public_read: Option<String>,
    api: bool,
    disabled: bool,
    no_create: bool,
    no_edit: bool,
    no_delete: bool,
    title_field: Option<String>,
}

/// Parse and validate the struct-level `#[model(...)]` attributes.
fn model_opts(item: &syn::ItemStruct, table: &TableDef) -> syn::Result<ModelOpts> {
    let mut opts = ModelOpts::default();

    for attr in item.attrs.iter().filter(|a| a.path().is_ident("model")) {
        attr.parse_nested_meta(|meta| {
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
        })?;
    }

    Ok(opts)
}

#[derive(Default)]
struct FieldUi {
    list: bool,
    search: bool,
    filter: bool,
    textarea: bool,
    readonly: bool,
    hidden: Option<bool>,
}

/// Parse and validate the field-level `#[ui(...)]` attributes and produce one
/// `UiField` construction expression per database column (relations carry no
/// generated UI yet and reject `#[ui]`).
fn ui_fields(item: &syn::ItemStruct, table: &TableDef) -> syn::Result<Vec<TokenStream>> {
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
        let ui = field_ui(field, col)?;

        let name = &col.name;
        let (list, search, filter) = (ui.list, ui.search, ui.filter);
        let readonly = ui.readonly;
        // json/blob columns have no sensible generated rendering, so they
        // default to hidden unless the dev explicitly opts them in.
        let hidden = ui
            .hidden
            .unwrap_or(col.json || col.ty == SqlType::Blob);

        let is_select = col.is_enum && !has_orm_text_flag(field)?;
        let widget = widget_variant(col, ui.textarea, is_select);
        let options = if is_select {
            let inner = option_inner(&field.ty);
            quote! {
                ::core::option::Option::Some(|| {
                    <#inner>::VARIANTS.iter().map(|v| v.as_str()).collect()
                })
            }
        } else {
            quote!(::core::option::Option::None)
        };

        out.push(quote! {
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
        });
    }
    Ok(out)
}

/// Parse one field's `#[ui(...)]` attributes and validate every flag against
/// the column's type.
fn field_ui(field: &syn::Field, col: &ColumnDef) -> syn::Result<FieldUi> {
    let mut ui = FieldUi::default();
    let plain_text = col.ty == SqlType::Text && !col.is_enum && !col.json;

    for attr in field.attrs.iter().filter(|a| a.path().is_ident("ui")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("list") {
                ui.list = true;
            } else if meta.path.is_ident("search") {
                if !plain_text {
                    return Err(meta.error("search needs a plain text column (String)"));
                }
                ui.search = true;
            } else if meta.path.is_ident("filter") {
                if !col.is_enum && col.ty != SqlType::Boolean {
                    return Err(
                        meta.error("filter needs a DbEnum or bool column to enumerate")
                    );
                }
                ui.filter = true;
            } else if meta.path.is_ident("textarea") {
                if !plain_text {
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
/// offer as select options.
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
