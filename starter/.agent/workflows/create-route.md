---
description: How to create a new route in the web app
---

To create a new route, follow these steps to ensure consistency across the backend and frontend.

### 1. Create the Backend Service
Create a new file in `src/services/` (e.g., `src/services/my_service.rs`).

- Use `use crate::{...}` for common imports.
- Define a `serde::Serialize` struct for your template context if needed.
- Implement the handler function (usually `#[get("/path")]` or `#[post("/path")]`).
- Return `AppResult` (which is `Result<HttpResponse, AppError>`).
- Use `data.render_tpl("template_name", &context).await` to return the response.

**Simple GET Request (No Context):**
```rust
use crate::{get, AppData, AuthUser, Data, Responder};

#[get("/my-route")]
pub async fn get(data: Data<AppData>, _user: AuthUser) -> impl Responder {
    data.render("my_template").await
}
```

**Request with Database and Context:**
```rust
use crate::{
    actix_web::{get, web},
    AppData, AppResult, AuthUser, Serialize,
};

#[derive(Serialize)]
struct Context {
    pub title: String,
    pub items: Vec<String>,
}

#[get("/my-route")]
pub async fn get(data: web::Data<AppData>, user: AuthUser) -> AppResult {
    // Check permissions if needed
    if user.claims.role != "admin" {
        return Err(crate::AppError::NoAuth);
    }

    let items = sqlx::query_scalar!("SELECT name FROM my_table")
        .fetch_all(&data.db)
        .await?;

    let context = Context {
        title: "My Page".to_string(),
        items,
    };

    Ok(data.render_tpl("my_template", &context).await)
}
```

### 2. Create the Astro Page
Create a matching Astro page in `src/frontend/src/pages/my_template.astro`.

- Use the standard `Layout`, `Header`, and `Card` components.
- Use Tera tags (`{{ ... }}` or `{% ... %}`) wrapped in Astro `<Fragment set:html={...} />` for dynamic data.
- **Template Mapping:** `data.render_tpl("my_template", ...)` maps to `src/frontend/src/pages/my_template.astro`. Underscores in template names (e.g., `user_edit`) map to folders in Astro (`src/frontend/src/pages/user/edit.astro`).

**Astro Example (`src/frontend/src/pages/my_template.astro`):**
```astro
---
import Layout from "../layouts/Layout.astro";
import Header from "../components/Header.astro";
import Card from "../components/Card.astro";
---

<Layout title="My Page">
  <main class="grid place-items-center h-full">
    <div class="w-full">
      <Header backlink="/">My Page Title</Header>
      <Card>
        <ul class="space-y-2">
          <Fragment set:html={"{% for item in items %}"} />
          <li><Fragment set:html={"{{ item }}"} /></li>
          <Fragment set:html={"{% endfor %}"} />
        </ul>
      </Card>
    </div>
  </main>
</Layout>
```

### 3. Register the Route
Add the new service to `src/services/mod.rs`.

```rust
mod my_service;

pub fn configure(cfg: &mut web::ServiceConfig) {
    // ...
    cfg.service(my_service::get);
}
```

### 4. Error Handling
- Use `AppResult` for handlers that can fail.
- Use `?` to propagate database or other errors.
- Use `Err(AppError::NoAuth)` for permission issues.
- These are automatically rendered using `error.astro` or `noauth.astro`.