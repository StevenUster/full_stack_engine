//! syn-based parsing of `#[derive(Table)]` structs and `#[derive(DbEnum)]`
//! enums into the schema model. This is the *only* place `#[orm(...)]`
//! attribute semantics are defined — the derive macro and the CLI both call
//! into here, so they can never drift apart.

use quote::ToTokens;

use crate::error::Error;
use crate::model::{
    ColumnDef, DefaultValue, EnumDef, ForeignKey, OnDelete, RelationDef, Schema, SqlType, TableDef,
};

/// A parsed struct field is either a database column or a Prisma-style
/// relation field (`#[orm(relation = fk)]`), which carries no DDL.
enum ParsedField {
    Column(Box<ColumnDef>),
    Relation(RelationDef),
}

/// Parse a whole tables folder: `sources` is `(file name, file content)`,
/// where the file name is only used in error messages. Two passes — enums
/// first so struct fields can resolve them — then foreign keys are resolved
/// from struct names to table names.
pub fn parse_sources(sources: &[(String, String)]) -> Result<Schema, Error> {
    parse_sources_with_external(sources, &[])
}

/// Like [`parse_sources`], but with additional already-known tables (module
/// snapshots) available for foreign-key/relation resolution — so an app
/// table can `references(SomeModuleStruct)` a table it doesn't define. The
/// external tables are *not* part of the returned schema.
pub fn parse_sources_with_external(
    sources: &[(String, String)],
    external: &[TableDef],
) -> Result<Schema, Error> {
    let mut files = Vec::new();
    for (name, code) in sources {
        let file = syn::parse_file(code).map_err(|e| Error::new(format!("{name}: {e}")))?;
        files.push((name, file));
    }

    let mut enums: Vec<EnumDef> = Vec::new();
    for (name, file) in &files {
        for item in &file.items {
            if let syn::Item::Enum(e) = item
                && has_derive(&e.attrs, "DbEnum")
            {
                let def = enum_from_item(e).map_err(|e| Error::new(format!("{name}: {e}")))?;
                if enums.iter().any(|x| x.rust_name == def.rust_name) {
                    return Err(Error::new(format!(
                        "{name}: duplicate DbEnum {}",
                        def.rust_name
                    )));
                }
                enums.push(def);
            }
        }
    }
    enums.sort_by(|a, b| a.rust_name.cmp(&b.rust_name));

    let mut tables: Vec<TableDef> = Vec::new();
    for (name, file) in &files {
        for item in &file.items {
            if let syn::Item::Struct(s) = item
                && (has_derive(&s.attrs, "Table") || has_model_attr(&s.attrs))
            {
                let table = table_from_struct(s, Some(&enums))
                    .map_err(|e| Error::new(format!("{name}: {e}")))?;
                if tables.iter().any(|t| t.name == table.name) {
                    return Err(Error::new(format!(
                        "{name}: duplicate table {}",
                        table.name
                    )));
                }
                if external.iter().any(|t| t.name == table.name) {
                    return Err(Error::new(format!(
                        "{name}: table {} is already defined by a module — rename the \
                         struct or set #[orm(table = \"...\")]",
                        table.name
                    )));
                }
                tables.push(table);
            }
        }
    }
    tables.sort_by(|a, b| a.name.cmp(&b.name));

    // Resolve foreign-key targets: `references(Event)` names a struct; turn
    // it into the table name. A name that already matches a table (e.g. from
    // `fse init` introspection) passes through unchanged. External (module)
    // tables participate in resolution like local ones.
    let by_struct: Vec<(String, String)> = tables
        .iter()
        .chain(external)
        .map(|t| (t.struct_name.clone(), t.name.clone()))
        .collect();
    let table_names: Vec<String> = tables
        .iter()
        .chain(external)
        .map(|t| t.name.clone())
        .collect();
    for table in &mut tables {
        for col in &mut table.columns {
            if let Some(fk) = &mut col.references {
                if let Some((_, tn)) = by_struct.iter().find(|(s, _)| *s == fk.table) {
                    fk.table = tn.clone();
                } else if !table_names.contains(&fk.table) {
                    return Err(Error::new(format!(
                        "{}.{}: references unknown table/struct `{}`",
                        table.struct_name, col.name, fk.table
                    )));
                }
            }
        }
    }

    // Resolve each relation's target table from the table it joins through: the
    // FK column's (now table-resolved) reference. Also verify the relation's
    // declared target struct matches that foreign key's target.
    for table in &mut tables {
        let fk_targets: Vec<(String, String)> = table
            .columns
            .iter()
            .filter_map(|c| {
                c.references
                    .as_ref()
                    .map(|fk| (c.name.clone(), fk.table.clone()))
            })
            .collect();
        for rel in &mut table.relations {
            if let Some((_, target)) = fk_targets
                .iter()
                .find(|(name, _)| *name == rel.local_column)
            {
                rel.target_table = target.clone();
            }
        }
    }

    Ok(Schema { tables, enums })
}

/// Parse one `#[derive(DbEnum)]` enum. Variants must be unit variants; the
/// stored value is the snake_case variant name.
pub fn enum_from_item(item: &syn::ItemEnum) -> Result<EnumDef, Error> {
    let rust_name = item.ident.to_string();
    let mut values = Vec::new();
    for v in &item.variants {
        if !matches!(v.fields, syn::Fields::Unit) {
            return Err(Error::new(format!(
                "{rust_name}::{}: DbEnum variants must be unit variants",
                v.ident
            )));
        }
        values.push(to_snake_case(&v.ident.to_string()));
    }
    if values.is_empty() {
        return Err(Error::new(format!(
            "{rust_name}: DbEnum needs at least one variant"
        )));
    }
    Ok(EnumDef { rust_name, values })
}

/// Parse one `#[derive(Table)]` struct.
///
/// `enums` is the full set of known `DbEnum`s when parsing a whole folder
/// (CLI). Pass `None` in single-item contexts (the derive macro, which cannot
/// see other items): any unknown non-`json` type is then assumed to be a
/// `DbEnum` and DDL-only data (`check_in`) is left empty.
pub fn table_from_struct(
    item: &syn::ItemStruct,
    enums: Option<&[EnumDef]>,
) -> Result<TableDef, Error> {
    let struct_name = item.ident.to_string();

    let mut table_name: Option<String> = None;
    let mut composite_uniques: Vec<Vec<String>> = Vec::new();
    let mut composite_indexes: Vec<Vec<String>> = Vec::new();
    for attr in item.attrs.iter().filter(|a| a.path().is_ident("orm")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                table_name = Some(lit.value());
                Ok(())
            } else if meta.path.is_ident("unique") {
                composite_uniques.push(parse_struct_column_list(&meta, "unique")?);
                Ok(())
            } else if meta.path.is_ident("index") {
                composite_indexes.push(parse_struct_column_list(&meta, "index")?);
                Ok(())
            } else {
                Err(meta.error(
                    "unknown #[orm(...)] key on a struct; expected `table = \"...\"`, \
                     `unique(col, ...)` or `index(col, ...)`",
                ))
            }
        })
        .map_err(|e| Error::new(format!("{struct_name}: {e}")))?;
    }
    let name = table_name.unwrap_or_else(|| pluralize(&to_snake_case(&struct_name)));

    let syn::Fields::Named(fields) = &item.fields else {
        return Err(Error::new(format!(
            "{struct_name}: a Table struct needs named fields"
        )));
    };

    let mut columns = Vec::new();
    let mut relations = Vec::new();
    for field in &fields.named {
        match field_from_field(&struct_name, field, enums)? {
            ParsedField::Column(c) => columns.push(*c),
            ParsedField::Relation(r) => relations.push(r),
        }
    }

    // A relation joins through one of this table's own foreign-key columns, so
    // resolve its LEFT/INNER-ness (nullable FK → LEFT) and validate the column.
    for rel in &mut relations {
        let Some(col) = columns.iter().find(|c| c.name == rel.local_column) else {
            return Err(Error::new(format!(
                "{struct_name}.{}: relation column `{}` is not a field on this struct",
                rel.field, rel.local_column
            )));
        };
        if col.references.is_none() {
            return Err(Error::new(format!(
                "{struct_name}.{}: relation column `{}` has no #[orm(references(...))] — a relation \
                 must join through a foreign key",
                rel.field, rel.local_column
            )));
        }
        rel.nullable = col.nullable;
    }

    // Composite unique/index column lists must name real columns on this
    // table — checked here, once every field has been parsed.
    for cols in composite_uniques.iter().chain(composite_indexes.iter()) {
        for col in cols {
            if columns.iter().all(|c| &c.name != col) {
                return Err(Error::new(format!(
                    "{struct_name}: unique(...)/index(...) references unknown column `{col}`"
                )));
            }
        }
    }

    let table = TableDef {
        name,
        struct_name: struct_name.clone(),
        columns,
        relations,
        composite_uniques,
        composite_indexes,
    };
    if table.primary_key().is_empty() {
        return Err(Error::new(format!(
            "{struct_name}: no primary key — add an `id: i64` field or mark fields with #[orm(primary_key)]"
        )));
    }
    Ok(table)
}

/// Parses the parenthesized column list following a struct-level `unique`/
/// `index` key, e.g. the `(user_id, run_id)` in `#[orm(unique(user_id, run_id))]`.
fn parse_struct_column_list(
    meta: &syn::meta::ParseNestedMeta,
    key: &str,
) -> syn::Result<Vec<String>> {
    let mut cols = Vec::new();
    meta.parse_nested_meta(|m| {
        let Some(ident) = m.path.get_ident() else {
            return Err(m.error("expected a column name"));
        };
        cols.push(ident.to_string());
        Ok(())
    })?;
    if cols.is_empty() {
        return Err(meta.error(format!(
            "{key}(...) needs at least one column, e.g. {key}(a, b)"
        )));
    }
    Ok(cols)
}

/// Classify a struct field: a relation field (`#[orm(relation = fk)]`) carries
/// no DDL and joins to another table; anything else is a database column.
fn field_from_field(
    struct_name: &str,
    field: &syn::Field,
    enums: Option<&[EnumDef]>,
) -> Result<ParsedField, Error> {
    if let Some(rel) = relation_from_field(struct_name, field)? {
        return Ok(ParsedField::Relation(rel));
    }
    Ok(ParsedField::Column(Box::new(column_from_field(
        struct_name,
        field,
        enums,
    )?)))
}

/// A relation field is `#[orm(relation = fk_column)] name: Option<Target>`. It
/// must be `Option` (unloaded relations are `None`) and carry no other orm
/// keys. Returns `None` when the field is an ordinary column.
///
/// Parses each `#[orm(...)]` attribute's arguments as a plain
/// `Punctuated<Meta, Comma>` (the generic form: bare `unique`, `name = value`,
/// or `name(...)`) rather than probing key-by-key with `parse_nested_meta` —
/// every field is scanned here before we know whether it is a relation or an
/// ordinary column, so this must never partially consume a key's value (e.g.
/// `references(Target, on_delete = cascade)`) only to abandon it; `Meta`
/// parses each item fully regardless of which key it turns out to be.
fn relation_from_field(
    struct_name: &str,
    field: &syn::Field,
) -> Result<Option<RelationDef>, Error> {
    let field_name = field.ident.as_ref().expect("named field").to_string();
    let ctx = format!("{struct_name}.{field_name}");

    let mut local_column: Option<String> = None;
    let mut other_key = false;
    for attr in field.attrs.iter().filter(|a| a.path().is_ident("orm")) {
        let metas = attr
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .map_err(|e| Error::new(format!("{ctx}: {e}")))?;
        for meta in &metas {
            if meta.path().is_ident("relation") {
                let syn::Meta::NameValue(nv) = meta else {
                    return Err(Error::new(format!(
                        "{ctx}: expected `relation = fk_column`"
                    )));
                };
                let syn::Expr::Path(p) = &nv.value else {
                    return Err(Error::new(format!(
                        "{ctx}: relation value must be a column name"
                    )));
                };
                let Some(ident) = p.path.get_ident() else {
                    return Err(Error::new(format!(
                        "{ctx}: relation value must be a column name"
                    )));
                };
                local_column = Some(ident.to_string());
            } else {
                other_key = true;
            }
        }
    }
    let Some(local_column) = local_column else {
        return Ok(None);
    };
    if other_key {
        return Err(Error::new(format!(
            "{ctx}: a #[orm(relation = ...)] field takes no other orm keys"
        )));
    }

    let (nullable, inner) = unwrap_option(&field.ty);
    if !nullable {
        return Err(Error::new(format!(
            "{ctx}: a relation field must be `Option<Target>` (it is `None` until loaded)"
        )));
    }
    let Some(target_struct) = last_segment_ident(inner) else {
        return Err(Error::new(format!(
            "{ctx}: relation target must be a struct type"
        )));
    };

    Ok(Some(RelationDef {
        field: field_name,
        target_struct,
        // Resolved from local_column's foreign key in `parse_sources`.
        target_table: String::new(),
        local_column,
        // Resolved from the FK column's nullability in `table_from_struct`.
        nullable: false,
    }))
}

fn column_from_field(
    struct_name: &str,
    field: &syn::Field,
    enums: Option<&[EnumDef]>,
) -> Result<ColumnDef, Error> {
    let name = field.ident.as_ref().expect("named field").to_string();
    let ctx = format!("{struct_name}.{name}");

    let mut unique = false;
    let mut json = false;
    let mut text = false;
    let mut index = false;
    let mut explicit_pk = false;
    let mut default: Option<DefaultValue> = None;
    let mut references: Option<ForeignKey> = None;
    let mut renamed_from: Option<String> = None;

    for attr in field.attrs.iter().filter(|a| a.path().is_ident("orm")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("unique") {
                unique = true;
            } else if meta.path.is_ident("json") {
                json = true;
            } else if meta.path.is_ident("text") {
                text = true;
            } else if meta.path.is_ident("index") {
                index = true;
            } else if meta.path.is_ident("primary_key") {
                explicit_pk = true;
            } else if meta.path.is_ident("renamed_from") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                renamed_from = Some(lit.value());
            } else if meta.path.is_ident("default") {
                let expr: syn::Expr = meta.value()?.parse()?;
                default = Some(parse_default(&expr).map_err(|m| meta.error(m))?);
            } else if meta.path.is_ident("references") {
                let mut target: Option<String> = None;
                let mut on_delete: Option<OnDelete> = None;
                meta.parse_nested_meta(|m| {
                    if m.path.is_ident("on_delete") {
                        let ident: syn::Ident = m.value()?.parse()?;
                        on_delete = Some(match ident.to_string().as_str() {
                            "cascade" => OnDelete::Cascade,
                            "set_null" => OnDelete::SetNull,
                            "restrict" => OnDelete::Restrict,
                            other => {
                                return Err(m.error(format!(
                                    "unknown on_delete `{other}`; expected cascade, set_null or restrict"
                                )));
                            }
                        });
                    } else if let Some(ident) = m.path.get_ident() {
                        target = Some(ident.to_string());
                    } else {
                        return Err(m.error("expected a struct name, e.g. references(Event)"));
                    }
                    Ok(())
                })?;
                let Some(target) = target else {
                    return Err(meta.error("references(...) needs a target, e.g. references(Event)"));
                };
                references = Some(ForeignKey { table: target, column: "id".into(), on_delete });
            } else {
                return Err(meta.error(
                    "unknown #[orm(...)] key; expected unique, json, text, index, primary_key, default, references or renamed_from",
                ));
            }
            Ok(())
        })
        .map_err(|e| Error::new(format!("{ctx}: {e}")))?;
    }

    let (nullable, inner) = unwrap_option(&field.ty);
    let rust_type = inner.to_token_stream().to_string().replace(' ', "");

    let (ty, is_enum, check_in) = if json {
        (SqlType::Text, false, None)
    } else if matches!(rust_type.as_str(), "u64" | "usize" | "isize") {
        // SQLite INTEGER is i64 and sqlx-sqlite has no Encode impl for these
        // — without this check the failure would be a cryptic trait-bound
        // error deep inside generated code.
        return Err(Error::new(format!(
            "{ctx}: `{rust_type}` cannot be stored in SQLite (INTEGER is i64) — use i64"
        )));
    } else if text {
        // `#[orm(text)]`: stored TEXT via as_str()/FromStr, no CHECK — for
        // types whose value set the schema layer cannot see (e.g. a role
        // enum generated by an app macro).
        if native_sql_type(inner).is_some() {
            return Err(Error::new(format!(
                "{ctx}: #[orm(text)] is for non-native types (this one maps natively already)"
            )));
        }
        (SqlType::Text, true, None)
    } else if let Some(t) = native_sql_type(inner) {
        (t, false, None)
    } else if let Some(enums) = enums {
        let type_name = last_segment_ident(inner);
        match enums
            .iter()
            .find(|e| Some(e.rust_name.as_str()) == type_name.as_deref())
        {
            Some(e) => (SqlType::Text, true, Some(e.values.clone())),
            None => {
                return Err(Error::new(format!(
                    "{ctx}: unsupported type `{rust_type}` — use a native type, derive DbEnum on it, or mark the field #[orm(json)]"
                )));
            }
        }
    } else {
        // Single-item context (derive macro): assume DbEnum, DDL data absent.
        (SqlType::Text, true, None)
    };

    let mut primary_key = explicit_pk;
    if name == "id" && ty == SqlType::Integer && !nullable {
        primary_key = true;
    }
    if primary_key && nullable {
        return Err(Error::new(format!("{ctx}: a primary key cannot be Option")));
    }

    if let Some(d) = &default {
        validate_default(&ctx, ty, d, check_in.as_deref())?;
    }
    if index && (unique || primary_key) {
        return Err(Error::new(format!(
            "{ctx}: #[orm(index)] is redundant — unique/primary key columns are already indexed"
        )));
    }

    Ok(ColumnDef {
        name,
        rust_type,
        ty,
        nullable,
        primary_key,
        unique,
        json,
        is_enum,
        index,
        default,
        references,
        check_in,
        renamed_from,
    })
}

fn parse_default(expr: &syn::Expr) -> Result<DefaultValue, String> {
    match expr {
        syn::Expr::Path(p) if p.path.is_ident("now") => Ok(DefaultValue::Now),
        syn::Expr::Lit(l) => match &l.lit {
            syn::Lit::Int(i) => Ok(DefaultValue::Int(
                i.base10_parse().map_err(|e| e.to_string())?,
            )),
            syn::Lit::Float(f) => Ok(DefaultValue::Float(
                f.base10_parse().map_err(|e| e.to_string())?,
            )),
            syn::Lit::Str(s) => Ok(DefaultValue::Text(s.value())),
            syn::Lit::Bool(b) => Ok(DefaultValue::Bool(b.value)),
            _ => Err("unsupported default literal".into()),
        },
        syn::Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => match parse_default(&u.expr)? {
            DefaultValue::Int(i) => Ok(DefaultValue::Int(-i)),
            DefaultValue::Float(f) => Ok(DefaultValue::Float(-f)),
            _ => Err("cannot negate this default".into()),
        },
        _ => Err("expected a literal or `now`, e.g. default = 0 or default = now".into()),
    }
}

fn validate_default(
    ctx: &str,
    ty: SqlType,
    d: &DefaultValue,
    check_in: Option<&[String]>,
) -> Result<(), Error> {
    let ok = matches!(
        (ty, d),
        (SqlType::Integer, DefaultValue::Int(_))
            | (SqlType::Real, DefaultValue::Float(_))
            | (SqlType::Real, DefaultValue::Int(_))
            | (SqlType::Text, DefaultValue::Text(_))
            | (SqlType::Boolean, DefaultValue::Bool(_))
            | (SqlType::Timestamp, DefaultValue::Now)
    );
    if !ok {
        return Err(Error::new(format!(
            "{ctx}: default {d:?} does not fit column type {ty:?}"
        )));
    }
    if let (Some(values), DefaultValue::Text(s)) = (check_in, d)
        && !values.iter().any(|v| v == s)
    {
        return Err(Error::new(format!(
            "{ctx}: default '{s}' is not one of the enum values {values:?}"
        )));
    }
    Ok(())
}

/// Map a Rust type to its native SQLite storage type. Anything not listed
/// here needs `#[orm(json)]` or a `DbEnum`.
pub fn native_sql_type(ty: &syn::Type) -> Option<SqlType> {
    let seg = last_segment(ty)?;
    Some(match seg.ident.to_string().as_str() {
        // u64/usize/isize are rejected with a dedicated error in
        // `column_from_field` — sqlx-sqlite cannot encode them.
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" => SqlType::Integer,
        "f32" | "f64" => SqlType::Real,
        "bool" => SqlType::Boolean,
        "String" => SqlType::Text,
        "Uuid" => SqlType::Text,
        "NaiveDateTime" | "DateTime" => SqlType::Timestamp,
        "NaiveDate" | "NaiveTime" => SqlType::Text,
        "Vec" => {
            if let syn::PathArguments::AngleBracketed(args) = &seg.arguments
                && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
                && last_segment(inner).is_some_and(|s| s.ident == "u8")
            {
                SqlType::Blob
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

fn unwrap_option(ty: &syn::Type) -> (bool, &syn::Type) {
    if let Some(seg) = last_segment(ty)
        && seg.ident == "Option"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return (true, inner);
    }
    (false, ty)
}

fn last_segment(ty: &syn::Type) -> Option<&syn::PathSegment> {
    if let syn::Type::Path(p) = ty {
        p.path.segments.last()
    } else {
        None
    }
}

fn last_segment_ident(ty: &syn::Type) -> Option<String> {
    last_segment(ty).map(|s| s.ident.to_string())
}

/// Does the item carry the framework's `#[model(...)]` attribute macro? Such
/// a struct expands to `#[derive(Table)]` plus app metadata, so the schema
/// layer treats it exactly like a hand-derived table. (The framework crates
/// are not a dependency here — this is a purely syntactic check, in the same
/// spirit as [`has_derive`].)
pub fn has_model_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .is_some_and(|s| s.ident == "model")
    })
}

/// Does `#[derive(...)]` on this item mention `name` (by last path segment,
/// so `fse_orm::Table` matches too)?
pub fn has_derive(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("derive"))
        .any(|a| {
            let mut found = false;
            let _ = a.parse_nested_meta(|meta| {
                if meta.path.segments.last().is_some_and(|s| s.ident == name) {
                    found = true;
                }
                Ok(())
            });
            found
        })
}

pub fn to_snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Naive English pluralization for default table names — struct `Category`
/// becomes table `categories`. Wrong for irregular nouns; override with
/// `#[orm(table = "...")]`.
pub fn pluralize(s: &str) -> String {
    if let Some(stem) = s.strip_suffix('y')
        && stem.chars().last().is_some_and(|c| !"aeiou".contains(c))
    {
        return format!("{stem}ies");
    }
    if s.ends_with('s')
        || s.ends_with('x')
        || s.ends_with('z')
        || s.ends_with("ch")
        || s.ends_with("sh")
    {
        return format!("{s}es");
    }
    format!("{s}s")
}
