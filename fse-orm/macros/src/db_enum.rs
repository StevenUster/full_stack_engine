//! `#[derive(DbEnum)]` expansion: a fieldless enum stored as TEXT. The
//! stored value is the snake_case variant name, and the same string is used
//! for serde, `FromStr` (form input) and `Display`, so the database, JSON
//! contexts and forms always agree.

use proc_macro2::TokenStream;
use quote::quote;

pub fn expand(item: &syn::ItemEnum) -> syn::Result<TokenStream> {
    let def = fse_schema::parse::enum_from_item(item)
        .map_err(|e| syn::Error::new(item.ident.span(), e.message))?;

    let name = &item.ident;
    let idents: Vec<&syn::Ident> = item.variants.iter().map(|v| &v.ident).collect();
    let values: Vec<&str> = def.values.iter().map(String::as_str).collect();

    Ok(quote! {
        impl #name {
            /// All variants, e.g. for rendering a `<select>`.
            pub const VARIANTS: &'static [#name] = &[#(#name::#idents),*];

            /// The stored TEXT value (snake_case variant name).
            pub fn as_str(&self) -> &'static str {
                match self {
                    #(#name::#idents => #values,)*
                }
            }
        }

        impl ::std::str::FromStr for #name {
            type Err = ::std::string::String;

            fn from_str(s: &str) -> ::std::result::Result<Self, Self::Err> {
                match s {
                    #(#values => Ok(#name::#idents),)*
                    other => Err(format!(
                        "unknown {} value: {other}",
                        stringify!(#name)
                    )),
                }
            }
        }

        impl ::std::fmt::Display for #name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::sqlx::Type<::sqlx::Sqlite> for #name {
            fn type_info() -> ::sqlx::sqlite::SqliteTypeInfo {
                <&str as ::sqlx::Type<::sqlx::Sqlite>>::type_info()
            }

            fn compatible(ty: &::sqlx::sqlite::SqliteTypeInfo) -> bool {
                <&str as ::sqlx::Type<::sqlx::Sqlite>>::compatible(ty)
            }
        }

        impl<'q> ::sqlx::Encode<'q, ::sqlx::Sqlite> for #name {
            fn encode_by_ref(
                &self,
                buf: &mut <::sqlx::Sqlite as ::sqlx::Database>::ArgumentBuffer<'q>,
            ) -> ::std::result::Result<::sqlx::encode::IsNull, ::sqlx::error::BoxDynError> {
                buf.push(::sqlx::sqlite::SqliteArgumentValue::Text(
                    ::std::borrow::Cow::Borrowed(self.as_str()),
                ));
                Ok(::sqlx::encode::IsNull::No)
            }
        }

        impl<'r> ::sqlx::Decode<'r, ::sqlx::Sqlite> for #name {
            fn decode(
                value: ::sqlx::sqlite::SqliteValueRef<'r>,
            ) -> ::std::result::Result<Self, ::sqlx::error::BoxDynError> {
                let s = <&str as ::sqlx::Decode<::sqlx::Sqlite>>::decode(value)?;
                s.parse().map_err(|e: ::std::string::String| e.into())
            }
        }

        impl ::serde::Serialize for #name {
            fn serialize<S: ::serde::Serializer>(
                &self,
                serializer: S,
            ) -> ::std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for #name {
            fn deserialize<D: ::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> ::std::result::Result<Self, D::Error> {
                let s = <::std::string::String as ::serde::Deserialize>::deserialize(deserializer)?;
                s.parse().map_err(::serde::de::Error::custom)
            }
        }
    })
}
