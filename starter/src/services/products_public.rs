//! The canonical **override example**: the public product catalog.
//!
//! `Product`'s admin CRUD is fully generated (`/admin/products`), but the
//! public pages carry a business rule generation can't know — only
//! `published` products are visible. So these two routes are hand-written;
//! they own `/products` and `/products/{slug}` simply by being registered
//! before the generated routes would be (app > modules > generated).

use crate::{
    AppData, AppResult, AppRole, Deserialize,
    actix_web::{HttpResponse, get, web},
    find_one, find_page,
};

use crate::models::product::{Product, ProductStatus};

const PER_PAGE: i64 = 20;

#[derive(Deserialize, Default)]
pub struct ProductSearchParams {
    pub search: Option<String>,
    pub page: Option<i64>,
}

/// `GET /products` — public catalog: only `published` products, searchable
/// by name, paginated.
#[get("/products")]
pub async fn get_public_products(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    query: web::Query<ProductSearchParams>,
) -> AppResult {
    use super::RenderTplExt;

    let page = query.page.unwrap_or(1).max(1);
    let search = query.search.as_deref().unwrap_or("").trim().to_string();

    let result = find_page!(
        Product,
        &data.db,
        status == ProductStatus::Published && name.contains_opt(&search),
        order_by: created_at.desc(),
        page: page,
        per_page: PER_PAGE
    )
    .await?;

    let total_pages = ((result.total + PER_PAGE - 1) / PER_PAGE).max(1);

    let rows: Vec<crate::serde_json::Value> = result
        .rows
        .into_iter()
        .map(|p| {
            crate::json!({
                "id": p.id,
                "name": p.name,
                "slug": p.slug,
                "description": p.description.unwrap_or_default(),
                "price": format!("{:.2}", p.price),
            })
        })
        .collect();

    Ok(req
        .render_tpl(
            "products",
            &crate::json!({
                "products": rows,
                "search": search,
                "page": page,
                "total_pages": total_pages,
                "total_count": result.total,
                "per_page": PER_PAGE,
            }),
        )
        .await)
}

#[derive(Deserialize, Default)]
pub struct ProductDetailQuery {
    pub ordered: Option<String>,
}

/// `GET /products/{slug}` — public product detail, with an order form for
/// signed-in users.
#[get("/products/{slug}")]
pub async fn get_public_product_detail(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    path: web::Path<String>,
    query: web::Query<ProductDetailQuery>,
) -> AppResult {
    use super::RenderTplExt;
    let slug = path.into_inner();

    let product = find_one!(Product, &data.db, slug == slug.as_str()).await?;
    let product = match product {
        Some(p) if p.status == ProductStatus::Published => p,
        _ => return Ok(HttpResponse::NotFound().finish()),
    };

    Ok(req
        .render_tpl(
            "products/detail",
            &crate::json!({
                "product": {
                    "id": product.id,
                    "name": product.name,
                    "slug": product.slug,
                    "description": product.description.unwrap_or_default(),
                    "price": format!("{:.2}", product.price),
                },
                "is_logged_in": crate::read_jwt::<AppRole>(&req).is_ok(),
                "ordered": query.ordered.as_deref() == Some("1"),
            }),
        )
        .await)
}
