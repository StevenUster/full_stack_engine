/**
 * fse-ssr Astro integration.
 *
 * Astro compiles each .astro file to a JS module whose template is a tagged
 * template literal — conditionals, comparisons and attribute values appear in
 * it as plain JS expressions. This integration rewrites those expressions to
 * the `__fse*` helpers exported from this package's `/ssr` entry, which keep
 * normal JavaScript semantics unless an SSR placeholder is involved, in which
 * case they emit Tera syntax into the built HTML.
 *
 * It also generates a `Translations` declaration-merge file from the app's
 * locale JSON so `t.*` accesses are type-checked per app.
 */
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import _generate from "@babel/generator";
import { parse } from "@babel/parser";
import _traverse from "@babel/traverse";
import * as t from "@babel/types";

const traverse = _traverse.default ?? _traverse;
const generate = _generate.default ?? _generate;

// Read once so the injected helper import always matches this package's
// actual published name, even if it's renamed later.
const PKG_NAME = JSON.parse(
  readFileSync(fileURLToPath(new URL("./package.json", import.meta.url)), "utf8"),
).name;
const SSR_SPECIFIER = `${PKG_NAME}/ssr`;

const HELPERS = [
  "__fseChunk",
  "__fseAddAttribute",
  "__fseBin",
  "__fseLogic",
  "__fseCond",
  "__fseNot",
  "__fseNullish",
  "__fseGuardIf",
];

const BIN_OPS = new Set(["===", "!==", "==", "!=", "<", "<=", ">", ">="]);

/** True if the expression awaits/yields at its own function level (thunking it would be invalid). */
function containsAwaitOrYield(path) {
  let found = false;
  path.traverse({
    AwaitExpression() {
      found = true;
    },
    YieldExpression() {
      found = true;
    },
    Function(p) {
      p.skip();
    },
  });
  return found;
}

export function transformAstroModule(code) {
  if (!code.includes("astro/compiler-runtime")) return null;

  const ast = parse(code, {
    sourceType: "module",
    plugins: ["typescript", "jsx"],
  });

  let renderLocal = null;
  let addAttributeLocal = null;
  for (const node of ast.program.body) {
    if (node.type === "ImportDeclaration" && node.source.value === "astro/compiler-runtime") {
      for (const spec of node.specifiers) {
        if (spec.type === "ImportSpecifier" && spec.imported.type === "Identifier") {
          if (spec.imported.name === "render") renderLocal = spec.local.name;
          if (spec.imported.name === "addAttribute") addAttributeLocal = spec.local.name;
        }
      }
    }
  }
  if (!renderLocal) return null;

  let used = false;
  const injected = new WeakSet();
  const helperCall = (name, args) => {
    used = true;
    const node = t.callExpression(t.identifier(name), args);
    injected.add(node);
    return node;
  };
  const thunk = (expr) => t.arrowFunctionExpression([], expr);

  traverse(ast, {
    // Wrap every `${…}` slot of the render template so proxies and markers
    // become raw Tera output instead of reaching Astro's renderer directly.
    TaggedTemplateExpression(path) {
      const tag = path.node.tag;
      if (tag.type !== "Identifier" || tag.name !== renderLocal) return;
      path.node.quasi.expressions = path.node.quasi.expressions.map((expr) =>
        injected.has(expr) ? expr : helperCall("__fseChunk", [expr]),
      );
    },
    CallExpression(path) {
      const callee = path.node.callee;
      if (
        addAttributeLocal &&
        callee.type === "Identifier" &&
        callee.name === addAttributeLocal &&
        !injected.has(path.node)
      ) {
        path.node.callee = t.identifier("__fseAddAttribute");
        injected.add(path.node);
        used = true;
      }
    },
    BinaryExpression(path) {
      if (!BIN_OPS.has(path.node.operator)) return;
      if (path.node.left.type === "PrivateName") return;
      path.replaceWith(
        helperCall("__fseBin", [
          t.stringLiteral(path.node.operator),
          path.node.left,
          path.node.right,
        ]),
      );
    },
    LogicalExpression(path) {
      if (containsAwaitOrYield(path.get("right"))) return;
      const { operator, left, right } = path.node;
      if (operator === "&&" || operator === "||") {
        path.replaceWith(
          helperCall("__fseLogic", [t.stringLiteral(operator), left, thunk(right)]),
        );
      } else if (operator === "??") {
        path.replaceWith(helperCall("__fseNullish", [left, thunk(right)]));
      }
    },
    ConditionalExpression(path) {
      if (
        containsAwaitOrYield(path.get("consequent")) ||
        containsAwaitOrYield(path.get("alternate"))
      ) {
        return;
      }
      path.replaceWith(
        helperCall("__fseCond", [
          path.node.test,
          thunk(path.node.consequent),
          thunk(path.node.alternate),
        ]),
      );
    },
    UnaryExpression(path) {
      if (path.node.operator !== "!") return;
      if (injected.has(path.node)) return;
      path.replaceWith(helperCall("__fseNot", [path.node.argument]));
    },
    // SSR values are only meaningful inside the template; a build-time `if`
    // testing one would silently take the truthy branch, so fail loudly.
    IfStatement(path) {
      if (injected.has(path.node.test)) return;
      path.node.test = helperCall("__fseGuardIf", [path.node.test]);
    },
  });

  if (!used) return null;

  ast.program.body.unshift(
    t.importDeclaration(
      HELPERS.map((h) => t.importSpecifier(t.identifier(h), t.identifier(h))),
      t.stringLiteral(SSR_SPECIFIER),
    ),
  );

  return generate(ast, { retainLines: false }, code);
}

function vitePlugin() {
  return {
    name: PKG_NAME,
    enforce: "post",
    transform(code, id) {
      // The bare .astro id is the compiled component; variants with a query
      // (?astro&type=script/style) are extracted assets and must stay untouched.
      if (!id.endsWith(".astro")) return null;
      const result = transformAstroModule(code);
      return result ? { code: result.code, map: result.map ?? null } : null;
    },
  };
}

function tsType(value) {
  if (typeof value === "string") return "string";
  if (typeof value === "number") return "number";
  if (typeof value === "boolean") return "boolean";
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const fields = Object.entries(value)
      .map(([k, v]) => `${JSON.stringify(k)}: ${tsType(v)};`)
      .join(" ");
    return `{ ${fields} }`;
  }
  return "unknown";
}

// Generated into the *consuming* project's `.astro/` (Astro's own convention
// for build-generated type files — gitignored, regenerated on every run),
// never into this package, since the shape is app-specific.
function generateTranslationTypes(rootUrl, localesPath, defaultLocale, logger) {
  const localeFile = fileURLToPath(new URL(`${localesPath}/${defaultLocale}.json`, rootUrl));
  const outFile = fileURLToPath(new URL("./.astro/fse-ssr-translations.d.ts", rootUrl));
  let locale;
  try {
    locale = JSON.parse(readFileSync(localeFile, "utf8"));
  } catch (err) {
    logger.warn(`Could not read ${localeFile}: ${err.message} — t.* will be untyped.`);
    locale = null;
  }
  const body = locale ? tsType(locale) : "Record<string, unknown>";
  // Declaration merging: augments the `Translations` interface this package
  // exports from its `/ssr` entry, instead of exporting an app-specific type
  // from a generically-published package.
  const content =
    `// Generated by ${PKG_NAME} from ${localesPath}/${defaultLocale}.json — do not edit.\n` +
    `import "${SSR_SPECIFIER}";\n\n` +
    `declare module "${SSR_SPECIFIER}" {\n` +
    `  interface Translations ${body}\n` +
    `}\n`;
  try {
    if (readFileSync(outFile, "utf8") === content) return;
  } catch {
    // First run: the file (and possibly `.astro/`) doesn't exist yet.
  }
  mkdirSync(dirname(outFile), { recursive: true });
  writeFileSync(outFile, content);
}

/**
 * Locates the theme's source directory. A `theme` starting with "." or "/"
 * is a local folder relative to the Astro project root; anything else is an
 * installed package name, resolved from the project.
 */
function resolveThemeDir(theme, rootUrl) {
  const root = fileURLToPath(rootUrl);
  if (theme.startsWith(".") || theme.startsWith("/")) {
    return resolve(root, theme);
  }
  const require = createRequire(join(root, "package.json"));
  return dirname(require.resolve(`${theme}/package.json`));
}

/** All page files under `dir`, as root-relative paths ("_model/list.astro"). */
function walkPages(dir, prefix = "") {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      out.push(...walkPages(join(dir, entry.name), rel));
    } else if (/\.(astro|md|mdx|html)$/.test(entry.name)) {
      out.push(rel);
    }
  }
  return out;
}

/** "foo/index.astro" → "/foo", "_model/list.astro" → "/_model/list". */
function routePattern(relPath) {
  let route = relPath.replace(/\.[^.]+$/, "");
  if (route === "index") return "/";
  route = route.replace(/\/index$/, "");
  return `/${route}`;
}

/**
 * Layered pages: every theme page the app does not define itself is added
 * to the build via `injectRoute`. Overriding a theme page = creating a file
 * with the same path under `src/pages/`. Theme layouts/components are
 * importable as `@theme/...`.
 */
function applyTheme(theme, config, injectRoute, updateConfig, logger) {
  const themeDir = resolveThemeDir(theme, config.root);
  const pagesDir = join(themeDir, "pages");
  const appPagesDir = fileURLToPath(new URL("./pages", config.srcDir));

  updateConfig({
    vite: { resolve: { alias: { "@theme": themeDir } } },
  });

  if (!existsSync(pagesDir)) {
    logger.warn(`Theme "${theme}" has no pages/ directory (${pagesDir}).`);
    return;
  }
  for (const rel of walkPages(pagesDir)) {
    const overridden = ["astro", "md", "mdx", "html"].some((ext) =>
      existsSync(join(appPagesDir, rel.replace(/\.[^.]+$/, `.${ext}`))),
    );
    if (overridden) continue;
    injectRoute({
      pattern: routePattern(rel),
      entrypoint: join(pagesDir, rel),
    });
  }
}

/**
 * @param {{ locales?: string, defaultLocale?: string, theme?: string }} [options]
 *   `locales`: path to the locale directory, relative to the Astro project
 *   root (default "../../locales" — the starter layout).
 *   `theme`: an installed theme package name (e.g. "fse-theme-default") or a
 *   local folder ("./themes/custom"). The theme's `pages/` fill in every
 *   route the app doesn't define; its files are importable as `@theme/...`.
 */
export default function fseSsr(options = {}) {
  const { locales = "../../locales", defaultLocale = "en", theme } = options;
  return {
    name: PKG_NAME,
    hooks: {
      "astro:config:setup": ({ config, updateConfig, injectRoute, logger }) => {
        generateTranslationTypes(config.root, locales, defaultLocale, logger);
        updateConfig({
          vite: {
            plugins: [vitePlugin()],
            resolve: {
              // Pin this package's own entries to absolute paths so imports
              // resolve from *anywhere* — theme/module sources typically live
              // outside the app tree (symlinked packages, extracted module
              // frontends) where node_modules lookup would fail.
              alias: {
                [`${PKG_NAME}/ssr`]: fileURLToPath(
                  new URL("./dist/runtime.js", import.meta.url),
                ),
                [`${PKG_NAME}/client`]: fileURLToPath(
                  new URL("./dist/client.js", import.meta.url),
                ),
              },
            },
          },
        });
        if (theme) {
          applyTheme(theme, config, injectRoute, updateConfig, logger);
        }
      },
    },
  };
}
