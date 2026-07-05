/**
 * fse-ssr build-time runtime.
 *
 * `ssr<T>()` returns proxies that stand in for server data while Astro
 * renders the static HTML. Interpolating a proxy emits the matching Tera
 * expression (`{{ path }}`); `.map()` emits a `{% for %}` loop. Comparisons
 * and conditionals can't be intercepted by proxies (JS operators aren't
 * overloadable), so the fse-ssr Astro integration rewrites the compiled
 * template's expressions to the `__fse*` helpers below, which fall back to
 * plain JavaScript semantics whenever no SSR placeholder is involved.
 *
 * This module runs only while Astro renders (build / dev server) — it never
 * ships to the browser. Island code reads values from `fse-ssr/client`.
 */
import {
  addAttribute as astroAddAttribute,
  markHTMLString,
} from "astro/runtime/server/index.js";

import type { GlobalSsrContext, Ssr } from "./types";

export type { GlobalSsrContext, Ssr, SsrList, SsrValue } from "./types";

const KIND = Symbol("fse-ssr.kind");
const EXPR = Symbol("fse-ssr.expr");

/**
 * Conditions are kept as trees because Tera has no general parentheses (it
 * only groups math), so precedence and negation must be resolved at emit
 * time (De Morgan, operator inversion) instead of with `(` `)`.
 */
type CondNode =
  | { k: "truthy"; expr: string }
  | { k: "cmp"; op: string; l: string; r: string }
  | { k: "and" | "or"; parts: CondNode[] }
  | { k: "not"; part: CondNode };

interface CondMarker {
  [KIND]: "cond";
  node: CondNode;
}

interface LogicMarker {
  [KIND]: "logic";
  op: "and" | "or";
  left: unknown;
  value: unknown;
}

interface TernaryMarker {
  [KIND]: "ternary";
  cond: CondNode;
  cons: unknown;
  alt: unknown;
}

interface BlockMarker {
  [KIND]: "block";
  parts: unknown[];
}

class FseSsrError extends Error {
  constructor(message: string) {
    super(`[fse-ssr] ${message}`);
    this.name = "FseSsrError";
  }
}

function kindOf(v: unknown): string | undefined {
  if (v === null || (typeof v !== "object" && typeof v !== "function")) {
    return undefined;
  }
  return (v as Record<symbol, string>)[KIND];
}

const isProxy = (v: unknown) => kindOf(v) === "value";
const isMarker = (v: unknown) => kindOf(v) !== undefined;

function exprOfProxy(v: unknown): string {
  return (v as Record<symbol, string>)[EXPR];
}

const raw = (s: string) => markHTMLString(s);

// ─── Tera expression helpers ────────────────────────────────────────────────

/** Serializes a build-time literal for use inside a Tera expression. */
function teraLiteral(v: unknown): string {
  if (isProxy(v)) return exprOfProxy(v);
  if (typeof v === "string") {
    if (!/^[^'\\\n\r]*$/.test(v)) {
      throw new FseSsrError(
        `String literal ${JSON.stringify(v)} can't be used in an SSR expression (quotes/backslashes are not supported).`,
      );
    }
    return `'${v}'`;
  }
  if (typeof v === "number" && Number.isFinite(v)) return String(v);
  if (typeof v === "boolean") return v ? "true" : "false";
  throw new FseSsrError(
    `Value of type ${typeof v} can't be used in an SSR expression.`,
  );
}

/**
 * Truthiness test for a bare value: guarded with `default(value=false)` so a
 * key that is absent from the request context reads as false instead of
 * failing the whole render.
 */
function bareCond(expr: string): string {
  return expr.includes("|") ? expr : `${expr} | default(value=false)`;
}

/** Condition tree for anything usable as a boolean. */
function condNodeOf(v: unknown): CondNode {
  switch (kindOf(v)) {
    case "value":
      return { k: "truthy", expr: bareCond(exprOfProxy(v)) };
    case "cond":
      return (v as CondMarker).node;
    case "logic": {
      const m = v as LogicMarker;
      return { k: m.op, parts: [condNodeOf(m.left), condNodeOf(m.value)] };
    }
    default:
      throw new FseSsrError(
        "This value can't be used as a server-side condition. Use an SSR value or a comparison of SSR values.",
      );
  }
}

const INVERTED_OP: Record<string, string> = {
  "==": "!=",
  "!=": "==",
  "<": ">=",
  ">=": "<",
  ">": "<=",
  "<=": ">",
};

function negate(n: CondNode): CondNode {
  switch (n.k) {
    case "truthy":
      return { k: "not", part: n };
    case "not":
      return n.part;
    case "cmp":
      return { ...n, op: INVERTED_OP[n.op] };
    case "and":
      return { k: "or", parts: n.parts.map(negate) };
    case "or":
      return { k: "and", parts: n.parts.map(negate) };
  }
}

function flatten(k: "and" | "or", parts: CondNode[]): CondNode[] {
  return parts.flatMap((p) => (p.k === k ? flatten(k, p.parts) : [p]));
}

/**
 * Emits a Tera condition. Tera's precedence matches JS (`and` binds tighter
 * than `or`), but there are no parentheses to override it, so an `or` nested
 * inside an `and` cannot be expressed and fails the build.
 */
function renderCond(n: CondNode): string {
  switch (n.k) {
    case "truthy":
      return n.expr;
    case "cmp":
      return `${n.l} ${n.op} ${n.r}`;
    case "not":
      return n.part.k === "truthy" ? `not ${n.part.expr}` : renderCond(negate(n.part));
    case "or":
      return flatten("or", n.parts).map(renderCond).join(" or ");
    case "and": {
      const parts = flatten("and", n.parts);
      if (parts.some((p) => p.k === "or")) {
        throw new FseSsrError(
          "`(a || b) && c` can't be expressed in Tera (it has no grouping parentheses). " +
            "Distribute the condition, e.g. `(a && c) || (b && c)`, or compute a flag in the service.",
        );
      }
      return parts.map(renderCond).join(" and ");
    }
  }
}

/** Tera condition string for anything usable as a boolean. */
function condExprOf(v: unknown): string {
  return renderCond(condNodeOf(v));
}

// ─── Value proxies ──────────────────────────────────────────────────────────

const IDENT = /^[A-Za-z_][A-Za-z0-9_]*$/;
// A proxy coerced to a string and used as a computed key, e.g.
// `t.roles[row.role]` where `row.role` is itself an SSR value.
const COERCED_KEY = /^\{\{ (.+) \}\}$/;

let loopDepth = 0;

function valueProxy(expr: string): unknown {
  return new Proxy(
    {},
    {
      get(_target, key) {
        if (key === KIND) return "value";
        if (key === EXPR) return expr;
        if (key === Symbol.toPrimitive || key === "toString" || key === "valueOf") {
          return () => `{{ ${expr} }}`;
        }
        // Safety net: if a proxy reaches Astro's renderer unwrapped, Astro
        // treats anything tagged HTMLString as raw markup and stringifies it,
        // which yields the correct `{{ path }}` output.
        if (key === Symbol.toStringTag) return "HTMLString";
        if (typeof key === "symbol") return undefined;
        // Astro probes children for these; answering with a child proxy
        // would make it await / stream us.
        if (key === "then" || key === "getReader" || key === "toJSON") {
          return undefined;
        }
        if (key === "map") {
          return (cb: (item: unknown, index: unknown, list: unknown) => unknown) =>
            loopBlock(expr, cb);
        }
        if (key === "length") return valueProxy(`${expr} | length`);
        if (expr.includes("|")) {
          throw new FseSsrError(
            `Can't access ".${String(key)}" on the filtered expression "${expr}".`,
          );
        }
        const coerced = COERCED_KEY.exec(key);
        if (coerced) return valueProxy(`${expr}[${coerced[1]}]`);
        if (IDENT.test(key) || /^\d+$/.test(key)) {
          return valueProxy(expr === "" ? key : `${expr}.${key}`);
        }
        throw new FseSsrError(
          `"${key}" is not a valid SSR property name (on "${expr}").`,
        );
      },
      has(_target, key) {
        return typeof key !== "symbol" && key !== "then" && key !== "getReader";
      },
    },
  );
}

function block(parts: unknown[]): BlockMarker {
  return { [KIND]: "block", parts };
}

function loopBlock(
  listExpr: string,
  cb: (item: unknown, index: unknown, list: unknown) => unknown,
): BlockMarker {
  const name = `it${loopDepth}`;
  loopDepth += 1;
  try {
    const body = cb(
      valueProxy(name),
      valueProxy("loop.index0"),
      valueProxy(listExpr),
    );
    return block([raw(`{% for ${name} in ${listExpr} %}`), body, raw("{% endfor %}")]);
  } finally {
    loopDepth -= 1;
  }
}

/**
 * Typed SSR placeholders for the page's render context. Interpolate them in
 * the template like ordinary values; the built HTML contains the matching
 * Tera expressions and the server substitutes the real data per request.
 */
export function ssr<T = Record<string, never>>(): Ssr<T & GlobalSsrContext> {
  return valueProxy("") as Ssr<T & GlobalSsrContext>;
}

// ─── Helpers injected by the build-time transform ───────────────────────────
// Each helper preserves plain JavaScript semantics when no SSR placeholder is
// involved, so rewriting every expression in a compiled .astro module is safe.

/** Renders one interpolated template chunk. */
export function __fseChunk(v: unknown): unknown {
  switch (kindOf(v)) {
    case undefined:
      return v;
    case "value":
      return raw(`{{ ${exprOfProxy(v)} }}`);
    case "block":
      return (v as BlockMarker).parts.map(__fseChunk);
    case "logic": {
      const m = v as LogicMarker;
      if (m.op === "and") {
        return __fseChunk(
          block([raw(`{% if ${condExprOf(m.left)} %}`), m.value, raw("{% endif %}")]),
        );
      }
      // `{a || fallback}`: render `a` when truthy, the fallback otherwise.
      if (!isProxy(m.left)) {
        throw new FseSsrError(
          '"cond || fallback" needs an SSR value on the left to know what to render when it is truthy.',
        );
      }
      return __fseChunk(
        block([
          raw(`{% if ${condExprOf(m.left)} %}{{ ${exprOfProxy(m.left)} }}{% else %}`),
          m.value,
          raw("{% endif %}"),
        ]),
      );
    }
    case "ternary": {
      const m = v as TernaryMarker;
      return __fseChunk(
        block([
          raw(`{% if ${renderCond(m.cond)} %}`),
          m.cons,
          raw("{% else %}"),
          m.alt,
          raw("{% endif %}"),
        ]),
      );
    }
    default:
      throw new FseSsrError(
        "A bare comparison can't be rendered as content. Use `{cond && <...>}` or `{cond ? a : b}`.",
      );
  }
}

// Keep in sync with Astro's boolean-attribute list (runtime/server/render/util.js).
const BOOLEAN_ATTRS =
  /^(?:allowfullscreen|async|autofocus|autoplay|checked|controls|default|defer|disabled|disablepictureinpicture|disableremoteplayback|formnovalidate|hidden|inert|loop|muted|nomodule|novalidate|open|playsinline|readonly|required|reversed|scoped|seamless|selected|itemscope)$/i;

function attrText(v: unknown): string {
  if (isProxy(v)) return `{{ ${exprOfProxy(v)} }}`;
  if (typeof v === "string" || typeof v === "number" || typeof v === "boolean") {
    return String(v).replace(/&/g, "&#38;").replace(/"/g, "&#34;");
  }
  throw new FseSsrError(
    `Value of type ${typeof v} can't be rendered inside an SSR attribute.`,
  );
}

/** Drop-in replacement for Astro's `addAttribute` that understands SSR markers. */
export function __fseAddAttribute(
  value: unknown,
  key: string,
  shouldEscape?: boolean,
  tagName?: string,
): unknown {
  const kind = kindOf(value);
  if (kind === undefined) {
    if (key === "class:list" && Array.isArray(value)) {
      value = value.map((entry) =>
        isProxy(entry) ? `{{ ${exprOfProxy(entry)} }}` : entry,
      );
    }
    return astroAddAttribute(value, key, shouldEscape, tagName);
  }

  if (BOOLEAN_ATTRS.test(key)) {
    return raw(`{% if ${condExprOf(value)} %} ${key}{% endif %}`);
  }
  if (kind === "value") {
    return raw(` ${key}="{{ ${exprOfProxy(value)} }}"`);
  }
  if (kind === "ternary") {
    const m = value as TernaryMarker;
    return raw(
      ` ${key}="{% if ${renderCond(m.cond)} %}${attrText(m.cons)}{% else %}${attrText(m.alt)}{% endif %}"`,
    );
  }
  throw new FseSsrError(
    `This SSR expression can't be used for the "${key}" attribute.`,
  );
}

/** `a === b`, `a < b`, … — compiles to a Tera comparison when SSR values are involved. */
export function __fseBin(op: string, l: unknown, r: unknown): unknown {
  if (
    (isMarker(l) || isMarker(r)) &&
    l !== undefined &&
    l !== null &&
    r !== undefined &&
    r !== null
  ) {
    const tera: Record<string, string> = {
      "===": "==",
      "==": "==",
      "!==": "!=",
      "!=": "!=",
      "<": "<",
      "<=": "<=",
      ">": ">",
      ">=": ">=",
    };
    const teraOp = tera[op];
    if (!teraOp) {
      throw new FseSsrError(`Operator "${op}" is not supported on SSR values.`);
    }
    const marker: CondMarker = {
      [KIND]: "cond",
      node: { k: "cmp", op: teraOp, l: teraLiteral(l), r: teraLiteral(r) },
    };
    return marker;
  }
  /* eslint-disable eqeqeq */
  switch (op) {
    case "===":
      return l === r;
    case "!==":
      return l !== r;
    case "==":
      return l == r;
    case "!=":
      return l != r;
    case "<":
      return (l as number) < (r as number);
    case "<=":
      return (l as number) <= (r as number);
    case ">":
      return (l as number) > (r as number);
    case ">=":
      return (l as number) >= (r as number);
    default:
      throw new FseSsrError(`Unknown operator "${op}".`);
  }
  /* eslint-enable eqeqeq */
}

/** `a && b` / `a || b` with a lazily evaluated right side. */
export function __fseLogic(op: "&&" | "||", l: unknown, rThunk: () => unknown): unknown {
  if (isMarker(l)) {
    const marker: LogicMarker = {
      [KIND]: "logic",
      op: op === "&&" ? "and" : "or",
      left: l,
      value: rThunk(),
    };
    return marker;
  }
  if (op === "&&") return l ? rThunk() : l;
  return l ? l : rThunk();
}

/** `t ? a : b` — compiles to `{% if %}…{% else %}…{% endif %}` for SSR tests. */
export function __fseCond(
  test: unknown,
  consThunk: () => unknown,
  altThunk: () => unknown,
): unknown {
  if (isMarker(test)) {
    const marker: TernaryMarker = {
      [KIND]: "ternary",
      cond: condNodeOf(test),
      cons: consThunk(),
      alt: altThunk(),
    };
    return marker;
  }
  return test ? consThunk() : altThunk();
}

/** `!x` — compiles to `not (…)` for SSR values. */
export function __fseNot(v: unknown): unknown {
  if (isMarker(v)) {
    const marker: CondMarker = { [KIND]: "cond", node: negate(condNodeOf(v)) };
    return marker;
  }
  return !v;
}

/** `a ?? b` — compiles to Tera's `default` filter for SSR values. */
export function __fseNullish(l: unknown, rThunk: () => unknown): unknown {
  if (isProxy(l)) {
    return valueProxy(`${exprOfProxy(l)} | default(value=${teraLiteral(rThunk())})`);
  }
  if (isMarker(l)) {
    throw new FseSsrError('"??" is only supported directly on SSR values.');
  }
  return l ?? rThunk();
}

/** Guards build-time `if` statements against accidental SSR-value tests. */
export function __fseGuardIf(test: unknown): unknown {
  if (isMarker(test)) {
    throw new FseSsrError(
      "SSR values can't drive build-time `if` statements in frontmatter. " +
        "Move the condition into the template: {cond && <...>} or {cond ? a : b}.",
    );
  }
  return test;
}
