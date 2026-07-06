/// <reference path="../.astro/types.d.ts" />
/// <reference path="../.astro/fse-ssr-translations.d.ts" />

declare module "swagger-ui-dist/swagger-ui-es-bundle.js" {
  const SwaggerUIBundle: (config: Record<string, unknown>) => unknown;
  export default SwaggerUIBundle;
}