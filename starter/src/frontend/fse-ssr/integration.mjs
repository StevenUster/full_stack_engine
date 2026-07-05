/**
 * fse-ssr Astro integration.
 *
 * Astro compiles each .astro file to a JS module whose template is a tagged
 * template literal — conditionals, comparisons and attribute values appear in
 * it as plain JS expressions. This integration rewrites those expressions to
 * the `__fse*` helpers from fse-ssr/runtime.ts, which keep normal JavaScript
 * semantics unless an SSR placeholder is involved, in which case they emit
 * Tera syntax into the built HTML.
 *
 * It also generates `translations.generated.ts` from the app's locale file so
 * `t.*` accesses are type-checked.
 */
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import _generate from "@babel/generator";
import { parse } from "@babel/parser";
import _traverse from "@babel/traverse";
import * as t from "@babel/types";

const traverse = _traverse.default ?? _traverse;
const generate = _generate.default ?? _generate;

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
      t.stringLiteral("fse-ssr"),
    ),
  );

  return generate(ast, { retainLines: false }, code);
}

function vitePlugin() {
  return {
    name: "fse-ssr",
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

function generateTranslationTypes(rootUrl, localesPath, defaultLocale, logger) {
  const localeFile = fileURLToPath(new URL(`${localesPath}/${defaultLocale}.json`, rootUrl));
  const outFile = fileURLToPath(new URL("./translations.generated.ts", import.meta.url));
  let locale;
  try {
    locale = JSON.parse(readFileSync(localeFile, "utf8"));
  } catch (err) {
    logger.warn(`Could not read ${localeFile}: ${err.message} — t.* will be untyped.`);
    locale = null;
  }
  const body = locale ? tsType(locale) : "Record<string, never>";
  const content =
    `// Generated by fse-ssr from ${localesPath}/${defaultLocale}.json — do not edit.\n` +
    `export type Translations = ${body};\n`;
  try {
    if (readFileSync(outFile, "utf8") === content) return;
  } catch {
    // First run: the file doesn't exist yet.
  }
  writeFileSync(outFile, content);
}

/**
 * @param {{ locales?: string, defaultLocale?: string }} [options]
 *   `locales`: path to the locale directory, relative to the Astro project
 *   root (default "../../locales" — the starter layout).
 */
export default function fseSsr(options = {}) {
  const { locales = "../../locales", defaultLocale = "en" } = options;
  return {
    name: "fse-ssr",
    hooks: {
      "astro:config:setup": ({ config, updateConfig, logger }) => {
        generateTranslationTypes(config.root, locales, defaultLocale, logger);
        updateConfig({
          vite: {
            plugins: [vitePlugin()],
            resolve: {
              alias: {
                "fse-ssr/client": fileURLToPath(new URL("./client.ts", import.meta.url)),
                "fse-ssr": fileURLToPath(new URL("./runtime.ts", import.meta.url)),
              },
            },
          },
        });
      },
    },
  };
}
