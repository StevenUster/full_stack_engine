/// Macro that generates an `AppRole` enum with all trait impls from a single definition.
#[macro_export]
macro_rules! define_roles {
    ( $( ($variant:ident, $str:literal, admin: $is_admin:literal, none: $is_none:literal, perms: [ $($perm:literal),* ]) ),+ $(,)? ) => {

        #[derive(
            $crate::prelude::Serialize,
            $crate::prelude::Deserialize,
            Debug, Clone, Copy, PartialEq, Eq,
            ::sqlx::Type,
        )]
        #[sqlx(rename_all = "lowercase")]
        #[serde(rename_all = "lowercase")]
        pub enum AppRole {
            $( $variant ),+
        }

        impl AppRole {
            pub fn all() -> &'static [Self] {
                &[ $( Self::$variant ),+ ]
            }

            pub fn all_roles() -> Vec<$crate::prelude::serde_json::Value> {
                Self::all()
                    .iter()
                    .map(|r| {
                        let s = <Self as $crate::prelude::Role>::as_str(r);
                        $crate::prelude::serde_json::json!({
                            "value": s,
                            "label": s[..1].to_uppercase() + &s[1..],
                        })
                    })
                    .collect()
            }
        }

        impl $crate::prelude::Role for AppRole {
            fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $str ),+
                }
            }

            fn from_role_str(s: &str) -> Self {
                match s.to_lowercase().as_str() {
                    $( $str => Self::$variant, )+
                    _ => {
                        $( if $is_none { return Self::$variant; } )+
                        unreachable!()
                    }
                }
            }

            fn is_admin(&self) -> bool {
                match self {
                    $( Self::$variant => $is_admin ),+
                }
            }

            fn is_none(&self) -> bool {
                match self {
                    $( Self::$variant => $is_none ),+
                }
            }

            fn has_permission(&self, permission: &str) -> bool {
                if <Self as $crate::prelude::Role>::is_admin(self) { return true; }
                match self {
                    $( Self::$variant => matches!(permission, $( $perm )|* | ""), )+
                }
            }
        }

        impl std::fmt::Display for AppRole {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", <Self as $crate::prelude::Role>::as_str(self))
            }
        }
    };
}
