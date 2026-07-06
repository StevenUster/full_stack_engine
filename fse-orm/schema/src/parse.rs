//! syn-based parsing of `#[derive(Table)]` structs and `#[derive(DbEnum)]`
//! enums into the schema model. This is the *only* place `#[orm(...)]`
//! attribute semantics are defined — the derive macro and the CLI both call
//! into here, so they can never drift apart.

use quote::ToTokens;

use crate::error::Error;
use crate::model::{
    ColumnDef, DefaultValue, EnumDef, ForeignKey, OnDelete, Schema, SqlType, TableDef,
};

/// Parse a whole tables folder: `sources` is `(file name, file content)`,
/// where the file name is only used in error messages. Two passes — enums
/// first so struct fields can resolve them — then foreign keys are resolved
/// from struct names to table names.
pub fn parse_sources(sources: &[(String, String)]) -> Result<Schema, Error> {
    let mut files = Vec::new();
    for (name, code) in sources {
        let file =
            syn::parse_file(code).map_err(|e| Error::new(format!("{name}: {e}")))?;
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
                    return Err(Error::new(format!("{name}: duplicate DbEnum {}", def.rust_name)));
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
                && has_derive(&s.attrs, "Table")
            {
                let table = table_from_struct(s, Some(&enums))
                    .map_err(|e| Error::new(format!("{name}: {e}")))?;
                if tables.iter().any(|t| t.name == table.name) {
                    return Err(Error::new(format!("{name}: duplicate table {}", table.name)));
                }
                tables.push(table);
            }
        }
    }
    tables.sort_by(|a, b| a.name.cmp(&b.name));

    // Resolve foreign-key targets: `references(Event)` names a struct; turn
    // it into the table name. A name that already matches a table (e.g. from
    // `fse init` introspection) passes through unchanged.
    let by_struct: Vec<(String, String)> = tables
        .iter()
        .map(|t| (t.struct_name.clone(), t.name.clone()))
        .collect();
    let table_names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
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
        return Err(Error::new(format!("{rust_name}: DbEnum needs at least one variant")));
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
    for attr in item.attrs.iter().filter(|a| a.path().is_ident("orm")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                table_name = Some(lit.value());
                Ok(())
            } else {
                Err(meta.error("unknown #[orm(...)] key on a struct; expected `table = \"...\"`"))
            }
        })
        .map_err(|e| Error::new(format!("{struct_name}: {e}")))?;
    }
    let name = table_name.unwrap_or_else(|| pluralize(&to_snake_case(&struct_name)));

    let syn::Fields::Named(fields) = &item.fields else {
        return Err(Error::new(format!("{struct_name}: a Table struct needs named fields")));
    };

    let mut columns = Vec::new();
    for field in &fields.named {
        columns.push(column_from_field(&struct_name, field, enums)?);
    }

    let table = TableDef { name, struct_name: struct_name.clone(), columns };
    if table.primary_key().is_empty() {
        return Err(Error::new(format!(
            "{struct_name}: no primary key — add an `id: i64` field or mark fields with #[orm(primary_key)]"
        )));
    }
    Ok(table)
}

fn column_from_field(
    struct_name: &str,
    field: &syn::Field,
    enums: Option<&[EnumDef]>,
) -> Result<ColumnDef, Error> {
    let name = field
        .ident
        .as_ref()
        .expect("named field")
        .to_string();
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
        match enums.iter().find(|e| Some(e.rust_name.as_str()) == type_name.as_deref()) {
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
            syn::Lit::Int(i) => Ok(DefaultValue::Int(i.base10_parse().map_err(|e| e.to_string())?)),
            syn::Lit::Float(f) => {
                Ok(DefaultValue::Float(f.base10_parse().map_err(|e| e.to_string())?))
            }
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
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "isize" | "usize" => {
            SqlType::Integer
        }
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
