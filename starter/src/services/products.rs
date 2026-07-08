//! Example manageable resource: a product catalog with a public listing +
//! detail page and an admin CRUD UI (`/product-manager/...`). Meant as a
//! template for "the thing your app actually manages" — copy this file,
//! rename `Product`/`products`, and adjust the fields in
//! `src/tables/product.rs`.

use crate::{
    AppData, AppError, AppResult, AppRole, AuthUser, Deserialize, Serialize,
    actix_web::{HttpResponse, delete, get, post, web},
    find_one, find_page, insert, update,
};

use crate::tables::product::{Product, ProductStatus};

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

#[derive(Serialize)]
struct ProductManagerRow {
    id: i64,
    name: String,
    price: String,
    status: &'static str,
    created_at: String,
    link: String,
    delete_url: String,
}

/// `GET /product-manager` — admin catalog table (search + pagination).
#[get("/product-manager")]
pub async fn get_products(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    query: web::Query<ProductSearchParams>,
) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("products.read")?;

    let page = query.page.unwrap_or(1).max(1);
    let search = query.search.as_deref().unwrap_or("").trim().to_string();

    let result = find_page!(
        Product,
        &data.db,
        name.contains_opt(&search),
        order_by: created_at.desc(),
        page: page,
        per_page: PER_PAGE
    )
    .await?;

    let total_pages = ((result.total + PER_PAGE - 1) / PER_PAGE).max(1);

    let rows: Vec<ProductManagerRow> = result
        .rows
        .into_iter()
        .map(|p| ProductManagerRow {
            id: p.id,
            name: p.name,
            price: format!("{:.2}", p.price),
            status: p.status.as_str(),
            created_at: p.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            link: format!("/product-manager/{}", p.id),
            delete_url: format!("/product-manager/{}", p.id),
        })
        .collect();

    Ok(req
        .render_tpl(
            "product-manager",
            &crate::json!({
                "rows": rows,
                "search": search,
                "page": page,
                "total_pages": total_pages,
                "total_count": result.total,
                "per_page": PER_PAGE,
            }),
        )
        .await)
}

#[get("/product-manager/create")]
pub async fn get_product_create(req: actix_web::HttpRequest, user: AuthUser<AppRole>) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("products.write")?;

    Ok(req
        .render_tpl(
            "product-manager/create",
            &crate::json!({
                "name": "",
                "slug": "",
                "description": "",
                "price": "",
                "error_slug": "",
            }),
        )
        .await)
}

#[derive(Deserialize, Serialize)]
pub struct ProductForm {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub price: f64,
    pub status: Option<String>,
}

fn generate_slug(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join("-")
}

#[post("/product-manager/create")]
pub async fn post_product_create(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    form: web::Form<ProductForm>,
) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("products.write")?;

    let slug = form
        .slug
        .clone()
        .unwrap_or_else(|| generate_slug(&form.name));

    if Product::fetch_by_slug(&data.db, &slug).await?.is_some() {
        return Ok(req
            .render_tpl(
                "product-manager/create",
                &crate::json!({
                    "name": form.name,
                    "slug": slug,
                    "description": form.description,
                    "price": format!("{:.2}", form.price),
                    "error_slug": "slug_taken",
                }),
            )
            .await);
    }

    insert!(
        Product,
        &data.db,
        name = form.name.clone(),
        slug = slug,
        description = form.description.clone(),
        price = form.price
    )
    .await?;

    Ok(HttpResponse::Found()
        .append_header(("Location", "/product-manager"))
        .finish())
}

#[get("/product-manager/{id}")]
pub async fn get_product(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("products.read")?;

    let product_id = path.into_inner();
    let product = Product::fetch(&data.db, product_id)
        .await?
        .ok_or_else(|| AppError::NotFound("product".to_string()))?;

    Ok(req
        .render_tpl(
            "product-manager/details",
            &crate::json!({
                "product": {
                    "id": product.id,
                    "name": product.name,
                    "slug": product.slug,
                    "description": product.description.unwrap_or_default(),
                    "price": format!("{:.2}", product.price),
                    "status": product.status,
                },
                "error_slug": "",
            }),
        )
        .await)
}

#[post("/product-manager/{id}")]
pub async fn post_product(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
    form: web::Form<ProductForm>,
) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("products.write")?;

    let product_id = path.into_inner();
    let slug = form
        .slug
        .clone()
        .unwrap_or_else(|| generate_slug(&form.name));

    let current = Product::fetch(&data.db, product_id)
        .await?
        .ok_or_else(|| AppError::NotFound("product".to_string()))?;

    // Only accept input naming a real status; anything else keeps the
    // current one (the enum's FromStr rejects unknown values).
    let status = form
        .status
        .as_deref()
        .and_then(|s| s.parse::<ProductStatus>().ok())
        .unwrap_or(current.status);

    let taken = find_one!(Product, &data.db, slug == slug.as_str() && id != product_id).await?;
    if taken.is_some() {
        return Ok(req
            .render_tpl(
                "product-manager/details",
                &crate::json!({
                    "product": {
                        "id": product_id,
                        "name": form.name,
                        "slug": slug,
                        "description": form.description,
                        "price": format!("{:.2}", form.price),
                        "status": status,
                    },
                    "error_slug": "slug_taken",
                }),
            )
            .await);
    }

    update!(
        Product,
        &data.db,
        id == product_id;
        name = form.name.clone(),
        slug = slug.clone(),
        description = form.description.clone(),
        price = form.price,
        status = status
    )
    .await?;

    Ok(HttpResponse::Found()
        .append_header(("Location", format!("/product-manager/{product_id}")))
        .finish())
}

#[delete("/product-manager/{id}")]
pub async fn delete_product(
    data: web::Data<AppData>,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
) -> AppResult {
    user.require_permission("products.write")?;

    Product::delete(&data.db, path.into_inner()).await?;

    Ok(HttpResponse::Ok().finish())
}
