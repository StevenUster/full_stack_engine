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
 * locale JSON so `t.*` accesses are type-checked per app, and implements
 * theme inheritance: the project's `theme.json` names a parent theme, whose
 * pages are built into this project with this project's overrides of any
 * parent component/layout/style/asset applied (see `themeResolverPlugin`).
 */
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  realpathSync,
  writeFileSync,
} from "node:fs";
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
    // An empty section is filled per app (e.g. the framework's `models` and
    // `roles`), so generic pages may index it with runtime keys.
    if (Object.keys(value).length === 0) return "Record<string, any>";
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

/** Reads a `theme.json` manifest, or `null` when the directory has none. */
function readManifest(dir) {
  const file = join(dir, "theme.json");
  if (!existsSync(file)) return null;
  try {
    return JSON.parse(readFileSync(file, "utf8"));
  } catch (err) {
    throw new Error(`${PKG_NAME}: invalid ${file}: ${err.message}`);
  }
}

function realDir(dir) {
  try {
    return realpathSync(dir);
  } catch {
    return resolve(dir);
  }
}

/**
 * The theme chain this project builds, child first: the project itself,
 * then every ancestor named by `theme.json` `parent` fields. A parent is an
 * installed package resolved from the theme that extends it (so a theme's
 * own dependencies are found), or a path starting with "." or "/".
 * Ancestors without Astro sources (`src/`) end source-level inheritance —
 * the framework still falls back to their *built* templates at runtime.
 */
function resolveThemeChain(rootDir, logger) {
  const self = readManifest(rootDir);
  const chain = [
    { name: self?.name ?? "(app)", dir: realDir(rootDir), srcDir: realDir(join(rootDir, "src")) },
  ];
  let manifest = self;
  let fromDir = rootDir;
  const seen = new Set([chain[0].name]);
  while (manifest?.parent) {
    const parent = manifest.parent;
    let dir;
    if (parent.startsWith(".") || parent.startsWith("/")) {
      dir = resolve(fromDir, parent);
    } else {
      try {
        const require = createRequire(join(fromDir, "package.json"));
        dir = dirname(require.resolve(`${parent}/package.json`));
      } catch {
        throw new Error(
          `${PKG_NAME}: theme "${manifest.name}" extends "${parent}", which is not installed ` +
            `(add it as a dependency).`,
        );
      }
    }
    const parentManifest = readManifest(dir);
    const name = parentManifest?.name ?? parent;
    if (seen.has(name)) {
      throw new Error(`${PKG_NAME}: theme inheritance cycle at "${name}".`);
    }
    seen.add(name);
    if (!existsSync(join(dir, "src"))) {
      logger.info(
        `Theme "${name}" ships no Astro sources — its templates are inherited at runtime only.`,
      );
      break;
    }
    chain.push({ name, dir: realDir(dir), srcDir: realDir(join(dir, "src")) });
    manifest = parentManifest;
    fromDir = dir;
  }
  return chain;
}

const stripQuery = (id) => id.split("?")[0];

/** Index of the chain layer whose `src/` contains `file`, or -1. */
function layerOf(chain, file) {
  return chain.findIndex((layer) => file === layer.srcDir || file.startsWith(layer.srcDir + sep));
}

/**
 * WordPress-style template parts for Astro themes. When a file of an
 * ancestor theme imports another file of its own `src/` (a relative import
 * like `../components/Card.astro`), the most specific theme that has a file
 * at the same `src/`-relative path wins — so a child theme overrides a
 * parent's component, layout, stylesheet or asset just by creating it.
 *
 * `@parent/...` resolves to the `src/` of the importing theme's parent and
 * is never redirected, so an override can wrap the original it replaces.
 */
function themeResolverPlugin(chain) {
  return {
    name: `${PKG_NAME}:themes`,
    enforce: "pre",
    async resolveId(source, importer, options) {
      if (!importer || source.startsWith("\0") || importer.startsWith("\0")) return null;
      const importerFile = stripQuery(importer);
      const importerLayer = layerOf(chain, importerFile);

      if (source === "@parent" || source.startsWith("@parent/")) {
        const own = Math.max(importerLayer, 0);
        const parent = chain[own + 1];
        if (!parent) {
          this.error(`"${source}" imported from ${importerFile}, but theme "${chain[own].name}" has no parent with Astro sources.`);
        }
        const target = join(parent.srcDir, source.slice("@parent".length));
        return this.resolve(target, importer, { ...options, skipSelf: true });
      }

      // Only ancestor files are redirected; the project's own imports and
      // query-suffixed sub-requests (styles/scripts of one .astro) stay put.
      if (importerLayer <= 0 || source.includes("?")) return null;
      const resolved = await this.resolve(source, importer, { ...options, skipSelf: true });
      if (!resolved || resolved.external) return resolved;
      const file = stripQuery(resolved.id);
      const targetLayer = layerOf(chain, file);
      if (targetLayer < 0) return resolved;
      const rel = relative(chain[targetLayer].srcDir, file);
      for (let i = 0; i < importerLayer; i++) {
        const candidate = join(chain[i].srcDir, rel);
        if (existsSync(candidate)) return candidate;
      }
      return resolved;
    },
  };
}

/** All page files under `dir`, as root-relative paths ("fse/list.astro"). */
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

/** "foo/index.astro" → "/foo", "fse/list.astro" → "/fse/list". */
function routePattern(relPath) {
  let route = relPath.replace(/\.[^.]+$/, "");
  if (route === "index") return "/";
  route = route.replace(/\/index$/, "");
  return `/${route}`;
}

/**
 * One pages layer: injects every page under `pagesDir` whose route the
 * project doesn't define and no higher-priority layer has claimed yet.
 * Overriding = creating a same-path file under `src/pages/` (or in a more
 * specific theme).
 */
function injectPagesLayer(pagesDir, appPagesDir, claimed, injectRoute) {
  if (!existsSync(pagesDir)) return;
  for (const rel of walkPages(pagesDir)) {
    const pattern = routePattern(rel);
    if (claimed.has(pattern)) continue;
    claimed.add(pattern);
    const overridden = ["astro", "md", "mdx", "html"].some((ext) =>
      existsSync(join(appPagesDir, rel.replace(/\.[^.]+$/, `.${ext}`))),
    );
    if (overridden) continue;
    injectRoute({
      pattern,
      entrypoint: join(pagesDir, rel),
    });
  }
}

/**
 * Module layers: every `<modulesDir>/<name>/frontend/pages` (extracted by
 * `fse sync`), below every theme in priority, ordered by module name.
 */
function applyModules(modulesDir, rootUrl, appPagesDir, claimed, injectRoute) {
  const base = resolve(fileURLToPath(rootUrl), modulesDir);
  if (!existsSync(base)) return;
  for (const entry of readdirSync(base, { withFileTypes: true }).sort((a, b) =>
    a.name.localeCompare(b.name),
  )) {
    if (!entry.isDirectory()) continue;
    injectPagesLayer(
      join(base, entry.name, "frontend", "pages"),
      appPagesDir,
      claimed,
      injectRoute,
    );
  }
}

/**
 * @param {{ locales?: string, defaultLocale?: string, modulesDir?: string, inheritPages?: boolean }} [options]
 *   `locales`: path to the locale directory, relative to the project root
 *   (default "../locales" — a `theme/` folder next to the app's `locales/`).
 *   `modulesDir`: where `fse sync` extracts module frontends (default
 *   "../.fse/modules"). Module pages layer below every theme's.
 *   `inheritPages`: build the ancestors' pages into this theme with its
 *   overrides applied (default true). With `false` the build only contains
 *   the project's own pages and the framework serves everything else from
 *   the parent's built templates.
 *
 * The theme chain comes from the project's `theme.json`
 * (`{ "name": "my-theme", "parent": "fse-theme-default" }`); the manifest is
 * copied into the build output, which is what the framework loads.
 */
export default function fseSsr(options = {}) {
  const {
    locales = "../locales",
    defaultLocale = "en",
    modulesDir = "../.fse/modules",
    inheritPages = true,
  } = options;
  let rootDir;
  return {
    name: PKG_NAME,
    hooks: {
      "astro:config:setup": ({ config, updateConfig, injectRoute, logger }) => {
        rootDir = fileURLToPath(config.root);
        generateTranslationTypes(config.root, locales, defaultLocale, logger);
        const chain = resolveThemeChain(rootDir, logger);
        if (chain.length > 1) {
          logger.info(`Theme chain: ${chain.map((l) => l.name).join(" -> ")}`);
        }
        updateConfig({
          vite: {
            plugins: [themeResolverPlugin(chain), vitePlugin()],
            resolve: {
              // Pin this package's own entries to absolute paths so imports
              // resolve from *anywhere* — parent theme/module sources
              // typically live outside the project tree (symlinked packages,
              // extracted module frontends) where node_modules lookup fails.
              alias: {
                [`${PKG_NAME}/ssr`]: fileURLToPath(
                  new URL("./dist/runtime.js", import.meta.url),
                ),
                [`${PKG_NAME}/client`]: fileURLToPath(
                  new URL("./dist/client.js", import.meta.url),
                ),
              },
            },
            // The dev server may serve files of parent themes that live
            // outside the project (e.g. a linked package).
            server: { fs: { allow: [rootDir, ...chain.map((l) => l.dir)] } },
          },
        });
        // Page priority: project (file-based routing) > parent > grandparent
        // > modules.
        const appPagesDir = fileURLToPath(new URL("./pages", config.srcDir));
        const claimed = new Set();
        if (inheritPages) {
          for (const layer of chain.slice(1)) {
            injectPagesLayer(join(layer.srcDir, "pages"), appPagesDir, claimed, injectRoute);
          }
        }
        applyModules(modulesDir, config.root, appPagesDir, claimed, injectRoute);
      },
      "astro:build:done": ({ dir, logger }) => {
        const manifest = join(rootDir, "theme.json");
        if (existsSync(manifest)) {
          copyFileSync(manifest, join(fileURLToPath(dir), "theme.json"));
        } else {
          logger.warn(
            "No theme.json in the project root — the framework can't load this build as a theme.",
          );
        }
      },
    },
  };
}
